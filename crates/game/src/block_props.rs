//! Per-block properties beyond identity that the item system reads: a block's
//! manifest name and what breaking it yields. Parsed from the block manifest
//! into a [`BlockProps`] that the server, the native client and the offline
//! `LocalServer` install once, right after loading the registry, so every
//! side agrees. Bevy-free.
//!
//! Terrain is studs: how a material collides, how long it takes to dig and
//! how it is drawn is the stud material table's (`stud_material.rs`). A
//! `BlockId` names an item / build material, mapped to a stud material by
//! name (`stud_material_of_block`); replacing it by the stud material table
//! is open (stud world plan, D4).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::BlockId;

/// The non-default properties of the registry's blocks (maps keep it small
/// and comparable: two equal manifests give equal props).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockProps {
    /// Non-`Same` drops only.
    pub(crate) drops: BTreeMap<u16, crate::BlockDrop>,
    pub(crate) names: BTreeMap<u16, String>,
}

impl BlockProps {
    /// What the block yields when broken.
    pub fn drop(&self, id: BlockId) -> crate::BlockDrop {
        self.drops.get(&id.0).copied().unwrap_or_default()
    }

    /// The manifest name of the block.
    pub fn name_of(&self, id: BlockId) -> Option<&str> {
        self.names.get(&id.0).map(String::as_str)
    }

    /// The block called `name` in the manifest.
    pub fn id_named(&self, name: &str) -> Option<BlockId> {
        self.names.iter().find(|(_, n)| n.as_str() == name).map(|(id, _)| BlockId(*id))
    }
}

static PROPS: OnceLock<BlockProps> = OnceLock::new();

/// Installs the process-global props. Idempotent for equal props (every
/// registry load calls it); panics if a different set is installed after.
pub fn install_block_props(p: BlockProps) {
    let installed = PROPS.get_or_init(|| p.clone());
    assert_eq!(*installed, p, "conflicting block properties installed in one process");
}

/// The installed props, or the empty default (no names, every block drops
/// itself) when none were.
pub fn block_props() -> &'static BlockProps {
    static DEFAULT: BlockProps = BlockProps { drops: BTreeMap::new(), names: BTreeMap::new() };
    PROPS.get().unwrap_or(&DEFAULT)
}
