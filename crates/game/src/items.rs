//! Item identity. An `ItemId` below [`ITEM_BLOCK_LIMIT`] is the block with
//! that `BlockId` (a block item); ids from there up are the non-block items
//! of [`EXTRA_ITEMS`]. Block drops come from the manifest (`block_props()`).

use crate::{Block, BlockId, ItemStack, block_props};

/// Item ids below this are block items (the item's id is the `BlockId`).
pub const ITEM_BLOCK_LIMIT: u16 = 1024;
/// The most one inventory stack holds.
pub const MAX_STACK: u32 = 64;

/// Non-block items, in id order starting at [`ITEM_BLOCK_LIMIT`]. Append only.
pub const EXTRA_ITEMS: &[&str] = &["stick"];
pub const STICK: ItemId = ItemId(ITEM_BLOCK_LIMIT);

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Default,
    state_hash::StateHash,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct ItemId(pub u16);

impl ItemId {
    /// The block item of `block`.
    pub fn block(block: BlockId) -> ItemId {
        debug_assert!(block.0 < ITEM_BLOCK_LIMIT);
        ItemId(block.0)
    }

    /// The block this item places, or `None` for a non-block item.
    pub fn as_block(self) -> Option<BlockId> {
        (self.0 < ITEM_BLOCK_LIMIT).then_some(BlockId(self.0))
    }

    /// The id of the extra item called `name`.
    pub fn extra_named(name: &str) -> Option<ItemId> {
        EXTRA_ITEMS
            .iter()
            .position(|n| *n == name)
            .map(|i| ItemId(ITEM_BLOCK_LIMIT + i as u16))
    }
}

/// What a broken block yields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlockDrop {
    /// The block's own item.
    #[default]
    Same,
    Nothing,
    Item(ItemId),
}

/// The display name of an item: a block's manifest name, else the extra
/// table's; `"unknown"` for an id in neither.
pub fn item_name(item: ItemId) -> &'static str {
    match item.as_block() {
        Some(b) => block_props().name_of(b).unwrap_or("unknown"),
        None => EXTRA_ITEMS
            .get((item.0 - ITEM_BLOCK_LIMIT) as usize)
            .copied()
            .unwrap_or("unknown"),
    }
}

/// Whether `item` names a real block or extra item that players may hold.
/// Air and liquids are not items.
pub fn item_exists(item: ItemId) -> bool {
    match item.as_block() {
        Some(block) => {
            block != BlockId::AIR
                && block_props().name_of(block).is_some()
                && !block_props().is_liquid(block)
        }
        None => ((item.0 - ITEM_BLOCK_LIMIT) as usize) < EXTRA_ITEMS.len(),
    }
}

/// Resolve a manifest block name or extra item name to a real item.
pub fn item_named(name: &str) -> Option<ItemId> {
    block_props()
        .id_named(name)
        .map(ItemId::block)
        .or_else(|| ItemId::extra_named(name))
        .filter(|&item| item_exists(item))
}

/// All real item ids in stable id order (blocks, then extra items).
pub fn item_ids() -> Vec<ItemId> {
    let mut out: Vec<ItemId> = block_props()
        .names
        .keys()
        .copied()
        .map(BlockId)
        .map(ItemId::block)
        .filter(|&item| item_exists(item))
        .collect();
    out.extend((0..EXTRA_ITEMS.len()).map(|i| ItemId(ITEM_BLOCK_LIMIT + i as u16)));
    out
}

/// Client-facing metadata for one editor palette item, in stable item-id order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteEntry {
    pub item: ItemId,
    pub name: &'static str,
    /// `None` is an extra (non-block) item, which the client puts in Items.
    pub tab: Option<crate::CreativeTab>,
}

/// Every placeable item and the palette section assigned to it.
pub fn palette_entries() -> Vec<PaletteEntry> {
    item_ids()
        .into_iter()
        .map(|item| PaletteEntry {
            item,
            name: item_name(item),
            tab: item
                .as_block()
                .and_then(|block| block_props().creative_tab(block)),
        })
        .collect()
}

/// What breaking `block` yields (one item), by the manifest. Tree data in
/// `Block::data` never leaks into the item.
pub fn drop_of(block: Block) -> Option<ItemStack> {
    if block.id == BlockId::AIR {
        return None;
    }
    let item = match block_props().drop(block.id) {
        BlockDrop::Same => ItemId::block(block.id),
        BlockDrop::Nothing => return None,
        BlockDrop::Item(i) => i,
    };
    Some(ItemStack { item, count: 1 })
}
