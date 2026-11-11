//! Item ids and the manifest's drop data.
use game::{
    drop_of, install_block_props, item_name, BlockId, BlockRegistry, ItemId, ItemStack,
    ITEM_BLOCK_LIMIT, STICK,
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
    assert_eq!(drop_of(id("dirt")), one(dirt));
    assert_eq!(drop_of(id("grass")), one(dirt));
    assert_eq!(drop_of(id("oak_leaves")), None);
    assert_eq!(drop_of(id("oak_branch")), one(STICK));
    assert_eq!(drop_of(id("oak_log")), one(ItemId::block(id("oak_log"))));
    assert_eq!(drop_of(BlockId::AIR), None);
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
