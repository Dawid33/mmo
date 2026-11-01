//! Per-block properties beyond identity: whether a block collides (`solid`)
//! and how it is drawn (`render`). Parsed from the block manifest into a
//! [`BlockProps`] that the server, the native client and the offline
//! `LocalServer` install once, right after loading the registry, so the sim
//! (colliders) and the client agree. Unknown ids are solid and opaque, which
//! is what every block was before properties existed. Bevy-free.

use std::collections::BTreeSet;
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

/// The non-default properties of the registry's blocks (sets keep it small
/// and comparable: two equal manifests give equal props).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockProps {
    pub(crate) non_solid: BTreeSet<u16>,
    pub(crate) cutout: BTreeSet<u16>,
    pub(crate) half_collider: BTreeSet<u16>,
    pub(crate) trunk: BTreeSet<u16>,
}

impl BlockProps {
    /// Whether the block collides and blocks the edit ray's placement. Air
    /// is not a block here; unknown ids are solid.
    pub fn is_solid(&self, id: BlockId) -> bool {
        !self.non_solid.contains(&id.0)
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

    /// The grid the block's chisel collides on.
    pub fn collider_scale(&self, id: BlockId) -> ColliderScale {
        if self.half_collider.contains(&id.0) { ColliderScale::Half } else { ColliderScale::Voxel }
    }
}

static PROPS: OnceLock<BlockProps> = OnceLock::new();

/// Installs the process-global props. Idempotent for equal props (every
/// registry load calls it); panics if a different set is installed after.
pub fn install_block_props(p: BlockProps) {
    let installed = PROPS.get_or_init(|| p.clone());
    assert_eq!(*installed, p, "conflicting block properties installed in one process");
}

/// The installed props, or the all-solid, all-opaque default when none were.
pub fn block_props() -> &'static BlockProps {
    static DEFAULT: BlockProps = BlockProps {
        non_solid: BTreeSet::new(),
        cutout: BTreeSet::new(),
        half_collider: BTreeSet::new(),
        trunk: BTreeSet::new(),
    };
    PROPS.get().unwrap_or(&DEFAULT)
}
