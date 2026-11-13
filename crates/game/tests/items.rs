//! Item ids and the manifest's block hardness / drop data.
use game::{
    drop_of, install_block_props, item_name, Block, BlockId, BlockRegistry, ItemId, ItemStack,
    DEFAULT_HARDNESS_CENTI, ITEM_BLOCK_LIMIT, STICK,
};

const MANIFEST: &str = include_str!("../../../assets/blocks/blocks.ron");

fn registry() -> BlockRegistry {
    let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
    install_block_props(reg.props());
    reg
}

#[test]
fn drops_follow_manifest() {
    let reg = registry();
    let id = |n: &str| reg.id_of(n).unwrap();
    let one = |item: ItemId| Some(ItemStack { item, count: 1 });
    let dirt = ItemId::block(id("dirt"));
    assert_eq!(drop_of(Block::new(id("dirt"))), one(dirt));
    assert_eq!(drop_of(Block::new(id("grass"))), one(dirt));
    assert_eq!(drop_of(Block::new(id("oak_leaves"))), None);
    assert_eq!(drop_of(Block::new(id("oak_branch"))), one(STICK));
    let log = Block::with_tree_id(id("oak_log"), 77);
    assert_eq!(drop_of(log), one(ItemId::block(id("oak_log"))));
    assert_eq!(drop_of(Block::new(BlockId::AIR)), None);
}

#[test]
fn hardness_defaults() {
    let reg = registry();
    let p = game::block_props();
    let h = |n: &str| p.hardness_centi(reg.id_of(n).unwrap());
    assert_eq!(h("stone"), 150);
    assert_eq!(h("dirt"), 50);
    assert_eq!(h("granite"), 200);
    assert_eq!(h("spruce_log"), 200);
    assert_eq!(h("jungle_leaves"), 20);
    assert_eq!(h("glacier_ice"), 50);
    assert_eq!(h("gravel"), DEFAULT_HARDNESS_CENTI);
    assert_eq!(DEFAULT_HARDNESS_CENTI, 100);
}

#[test]
fn item_ids_and_names() {
    let reg = registry();
    let dirt = reg.id_of("dirt").unwrap();
    assert_eq!(ItemId::block(dirt).as_block(), Some(dirt));
    assert_eq!(STICK.as_block(), None);
    assert_eq!(STICK, ItemId(ITEM_BLOCK_LIMIT));
    assert_eq!(item_name(ItemId::block(dirt)), "dirt");
    assert_eq!(item_name(STICK), "stick");
}

#[test]
fn unknown_drop_name_fails_load() {
    let src = r#"(blocks:[
        (id:0,name:"air",textures:Untextured),
        (id:1,name:"dirt",textures:All("d.png"),drop:Item("nope")),
    ])"#;
    assert!(BlockRegistry::from_ron(src).is_err());
    let ok = r#"(blocks:[
        (id:0,name:"air",textures:Untextured),
        (id:1,name:"dirt",textures:All("d.png"),drop:Item("stick")),
        (id:2,name:"x",textures:All("d.png"),drop:Item("dirt")),
    ])"#;
    assert!(BlockRegistry::from_ron(ok).is_ok());
}
