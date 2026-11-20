//! Per-block properties beyond identity: whether a block collides (`solid`)
//! and how it is drawn (`render`). Parsed from the block manifest into a
//! [`BlockProps`] that the server, the native client and the offline
//! `LocalServer` install once, right after loading the registry, so the sim
//! (colliders) and the client agree. Unknown ids are solid and opaque, which
//! is what every block was before properties existed. Bevy-free.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use crate::BlockId;

/// How a block's faces are drawn. `Cutout` blocks (leaves) have texels with
/// alpha 0 that are discarded rather than blended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RenderMode {
    #[default]
    Opaque,
    Cutout,
}

/// What a block is made of, for movement and rays. `Liquid` blocks (water)
/// have no collider, the edit ray passes through them, and a build may
/// replace them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum BlockKind {
    #[default]
    Solid,
    Liquid,
}

/// Which grid a chiseled block's partial voxels collide on. `Voxel` (the
/// default) keeps every solid voxel; `Half` (tree wood) rounds each 8³
/// octant to one half-block cell, solid when at least `HALF_CELL_FILL_MIN`
/// of its voxels are — far fewer collider cells for a rounder shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ColliderScale {
    #[default]
    Voxel,
    Half,
}

/// Solid voxels (of an octant's 512) that make a `ColliderScale::Half`
/// block's octant one solid half-block cell.
pub const HALF_CELL_FILL_MIN: u32 = 128;

/// Break time (hundredths) of a block whose manifest entry has no `hardness`.
pub const DEFAULT_HARDNESS_CENTI: u16 = 100;

/// The non-default properties of the registry's blocks (sets keep it small
/// and comparable: two equal manifests give equal props).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockProps {
    pub(crate) non_solid: BTreeSet<u16>,
    pub(crate) cutout: BTreeSet<u16>,
    pub(crate) half_collider: BTreeSet<u16>,
    pub(crate) trunk: BTreeSet<u16>,
    /// Liquid ids; always also in `non_solid`.
    pub(crate) liquid: BTreeSet<u16>,
    /// Non-default hardness only.
    pub(crate) hardness: BTreeMap<u16, u16>,
    /// Non-`Same` drops only.
    pub(crate) drops: BTreeMap<u16, crate::BlockDrop>,
    pub(crate) names: BTreeMap<u16, String>,
    pub(crate) creative_tabs: BTreeMap<u16, crate::CreativeTab>,
}

impl BlockProps {
    /// Whether the block collides and blocks the edit ray's placement. Air
    /// is not a block here; unknown ids are solid.
    pub fn is_solid(&self, id: BlockId) -> bool {
        !self.non_solid.contains(&id.0)
    }

    /// Whether the block is a liquid (water).
    pub fn is_liquid(&self, id: BlockId) -> bool {
        self.liquid.contains(&id.0)
    }

    /// Every liquid block id, ascending (renderers build lookup tables from it).
    pub fn liquid_ids(&self) -> impl Iterator<Item = BlockId> + '_ {
        self.liquid.iter().map(|&id| BlockId(id))
    }

    /// Whether the edit / pick ray stops at the block: anything but air and
    /// liquids.
    pub fn blocks_ray(&self, id: BlockId) -> bool {
        id != BlockId::AIR && !self.is_liquid(id)
    }

    /// Whether the block renders with alpha-tested (cutout) texels.
    pub fn is_cutout(&self, id: BlockId) -> bool {
        self.cutout.contains(&id.0)
    }

    /// Every cutout block id, ascending (renderers build lookup tables from it).
    pub fn cutout_ids(&self) -> impl Iterator<Item = BlockId> + '_ {
        self.cutout.iter().map(|&id| BlockId(id))
    }

    /// Whether the block is tree trunk wood (a `<species>_log`, or a
    /// one-block species' block — `registry::SINGLE_BLOCK_TREES`): a chop on
    /// it cuts the trunk's cross-section (`trees::cut_blocks`).
    pub fn is_trunk(&self, id: BlockId) -> bool {
        self.trunk.contains(&id.0)
    }

    /// Break time in hundredths ([`DEFAULT_HARDNESS_CENTI`] unless set).
    pub fn hardness_centi(&self, id: BlockId) -> u16 {
        self.hardness
            .get(&id.0)
            .copied()
            .unwrap_or(DEFAULT_HARDNESS_CENTI)
    }

    /// What the block yields when broken.
    pub fn drop(&self, id: BlockId) -> crate::BlockDrop {
        self.drops.get(&id.0).copied().unwrap_or_default()
    }

    /// The manifest name of the block.
    pub fn name_of(&self, id: BlockId) -> Option<&str> {
        self.names.get(&id.0).map(String::as_str)
    }

    /// The block's editor-palette section, absent for air and liquids.
    pub fn creative_tab(&self, id: BlockId) -> Option<crate::CreativeTab> {
        self.creative_tabs.get(&id.0).copied()
    }

    /// The block called `name` in the manifest.
    pub fn id_named(&self, name: &str) -> Option<BlockId> {
        self.names
            .iter()
            .find(|(_, n)| n.as_str() == name)
            .map(|(id, _)| BlockId(*id))
    }

    /// The grid the block's chisel collides on.
    pub fn collider_scale(&self, id: BlockId) -> ColliderScale {
        if self.half_collider.contains(&id.0) {
            ColliderScale::Half
        } else {
            ColliderScale::Voxel
        }
    }
}

static PROPS: OnceLock<BlockProps> = OnceLock::new();

/// Installs the process-global props. Idempotent for equal props (every
/// registry load calls it); panics if a different set is installed after.
pub fn install_block_props(p: BlockProps) {
    let installed = PROPS.get_or_init(|| p.clone());
    assert_eq!(
        *installed, p,
        "conflicting block properties installed in one process"
    );
}

/// The installed props, `None` before [`install_block_props`] ran. Once
/// installed they never change, so anything derived from them may be cached.
pub fn installed_block_props() -> Option<&'static BlockProps> {
    PROPS.get()
}

/// The installed props, or the all-solid, all-opaque default when none were.
pub fn block_props() -> &'static BlockProps {
    static DEFAULT: BlockProps = BlockProps {
        non_solid: BTreeSet::new(),
        cutout: BTreeSet::new(),
        half_collider: BTreeSet::new(),
        trunk: BTreeSet::new(),
        liquid: BTreeSet::new(),
        hardness: BTreeMap::new(),
        drops: BTreeMap::new(),
        names: BTreeMap::new(),
        creative_tabs: BTreeMap::new(),
    };
    PROPS.get().unwrap_or(&DEFAULT)
}
