//! Data-driven block registry. The RON manifest (`assets/blocks/blocks.ron`)
//! is the source of truth for block identity: each `BlockDef` carries an
//! explicit, stable `id`. `BlockId` is the single material identity used at
//! both block and voxel granularity; the sim only distinguishes AIR from
//! solid. Texture specs are plain strings — the server ignores them, the
//! client resolves them to array-texture layers. Bevy-free.

use std::collections::BTreeMap;

/// Numeric material identity, assigned by the manifest. `AIR` is reserved.
/// Serialized (over the wire) and hashed, so client and server must agree.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    state_hash::StateHash,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct BlockId(pub u16);

impl BlockId {
    pub const AIR: BlockId = BlockId(0);
}

/// How a block's faces are textured. `Untextured` blocks (air) never mesh.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextureSpec {
    Untextured,
    All(String),
    Faces {
        top: String,
        side: String,
        bottom: String,
    },
}

/// One manifest entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockDef {
    pub id: u16,
    pub name: String,
    pub textures: TextureSpec,
    /// Whether the block collides. Default true.
    #[serde(default = "default_solid")]
    pub solid: bool,
    /// Solid (default) or liquid. A liquid never collides.
    #[serde(default)]
    pub kind: crate::BlockKind,
    /// How the block is drawn. Default opaque.
    #[serde(default)]
    pub render: crate::RenderMode,
    /// The grid a chiseled block collides on. Default voxel.
    #[serde(default)]
    pub collider: crate::ColliderScale,
    /// Break time, in hundredths (`DEFAULT_HARDNESS_CENTI` when absent).
    #[serde(default)]
    pub hardness: Option<u16>,
    /// What breaking it yields (`Same` when absent).
    #[serde(default)]
    pub drop: Option<DropDef>,
}

/// A manifest drop: item names are resolved (and checked) at registry load.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DropDef {
    Same,
    Nothing,
    Item(String),
}

fn default_solid() -> bool {
    true
}

/// Top-level RON document shape: `( blocks: [ ... ] )`.
#[derive(Debug, Clone, serde::Deserialize)]
struct BlockManifest {
    blocks: Vec<BlockDef>,
}

#[derive(Debug)]
pub enum RegistryError {
    Parse(String),
    DuplicateId(u16),
    DuplicateName(String),
    MissingAir,
    BadAir,
    UnknownDropItem { block: String, item: String },
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Parse(e) => write!(f, "malformed block manifest: {e}"),
            RegistryError::DuplicateId(id) => write!(f, "duplicate block id {id}"),
            RegistryError::DuplicateName(n) => write!(f, "duplicate block name {n:?}"),
            RegistryError::MissingAir => write!(f, "block manifest must define id 0 as air"),
            RegistryError::BadAir => write!(
                f,
                "block id 0 must be name \"air\" with Untextured textures"
            ),
            RegistryError::UnknownDropItem { block, item } => {
                write!(f, "block {block:?} drops unknown item {item:?}")
            }
        }
    }
}

impl std::error::Error for RegistryError {}

/// Tree species made of one block named after the species, not
/// `<species>_log` / `_branch` / `_leaves` (worldgen's `Species::single_block`):
/// their limb-0 blocks are trunk wood (`BlockProps::is_trunk`).
pub const SINGLE_BLOCK_TREES: &[&str] = &["cactus"];

/// Parsed, validated registry. Indexed by ascending `BlockId`.
#[derive(Debug, Clone, Default)]
pub struct BlockRegistry {
    defs: BTreeMap<BlockId, BlockDef>,
    by_name: BTreeMap<String, BlockId>,
}

impl BlockRegistry {
    pub fn from_ron(src: &str) -> Result<Self, RegistryError> {
        let manifest: BlockManifest = ron::Options::default()
            .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
            .from_str(src)
            .map_err(|e| RegistryError::Parse(e.to_string()))?;

        let mut defs = BTreeMap::new();
        let mut by_name = BTreeMap::new();
        for def in manifest.blocks {
            let id = BlockId(def.id);
            if by_name.insert(def.name.clone(), id).is_some() {
                return Err(RegistryError::DuplicateName(def.name));
            }
            if defs.insert(id, def).is_some() {
                return Err(RegistryError::DuplicateId(id.0));
            }
        }

        match defs.get(&BlockId::AIR) {
            None => return Err(RegistryError::MissingAir),
            Some(d) if d.name == "air" && d.textures == TextureSpec::Untextured => {}
            Some(_) => return Err(RegistryError::BadAir),
        }

        let reg = Self { defs, by_name };
        for def in reg.defs.values() {
            if let Some(DropDef::Item(name)) = &def.drop {
                if reg.drop_item(name).is_none() {
                    return Err(RegistryError::UnknownDropItem {
                        block: def.name.clone(),
                        item: name.clone(),
                    });
                }
            }
        }
        Ok(reg)
    }

    pub fn id_of(&self, name: &str) -> Option<BlockId> {
        self.by_name.get(name).copied()
    }

    /// The item called `name`: a block's item, else an extra item.
    fn drop_item(&self, name: &str) -> Option<crate::ItemId> {
        match self.id_of(name) {
            Some(b) => Some(crate::ItemId::block(b)),
            None => crate::ItemId::extra_named(name),
        }
    }

    /// The collision / render properties of every block, for
    /// [`crate::install_block_props`].
    pub fn props(&self) -> crate::BlockProps {
        let mut p = crate::BlockProps::default();
        for (id, def) in self.iter() {
            if def.kind == crate::BlockKind::Liquid {
                p.liquid.insert(id.0);
            }
            if !def.solid || def.kind == crate::BlockKind::Liquid {
                p.non_solid.insert(id.0);
            }
            if def.render == crate::RenderMode::Cutout {
                p.cutout.insert(id.0);
            }
            if def.collider == crate::ColliderScale::Half {
                p.half_collider.insert(id.0);
            }
            p.names.insert(id.0, def.name.clone());
            if let Some(h) = def.hardness.filter(|h| *h != crate::DEFAULT_HARDNESS_CENTI) {
                p.hardness.insert(id.0, h);
            }
            match &def.drop {
                None | Some(DropDef::Same) => {}
                Some(DropDef::Nothing) => {
                    p.drops.insert(id.0, crate::BlockDrop::Nothing);
                }
                Some(DropDef::Item(n)) => {
                    // Checked by `from_ron`.
                    let item = self.drop_item(n).expect("drop item validated at load");
                    p.drops.insert(id.0, crate::BlockDrop::Item(item));
                }
            }
            // Tree trunks are named `<species>_log` (worldgen's `TreeBlocks`),
            // or are a one-block species' block (a cactus is all trunk wood).
            if def.name.ends_with("_log") || SINGLE_BLOCK_TREES.contains(&def.name.as_str()) {
                p.trunk.insert(id.0);
            }
        }
        p
    }

    pub fn def(&self, id: BlockId) -> Option<&BlockDef> {
        self.defs.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (BlockId, &BlockDef)> {
        self.defs.iter().map(|(id, def)| (*id, def))
    }
}
