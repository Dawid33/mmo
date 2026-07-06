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
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default,
    serde::Serialize, serde::Deserialize,
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
    Faces { top: String, side: String, bottom: String },
}

/// One manifest entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockDef {
    pub id: u16,
    pub name: String,
    pub textures: TextureSpec,
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
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Parse(e) => write!(f, "malformed block manifest: {e}"),
            RegistryError::DuplicateId(id) => write!(f, "duplicate block id {id}"),
            RegistryError::DuplicateName(n) => write!(f, "duplicate block name {n:?}"),
            RegistryError::MissingAir => write!(f, "block manifest must define id 0 as air"),
            RegistryError::BadAir => write!(f, "block id 0 must be name \"air\" with Untextured textures"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Parsed, validated registry. Indexed by ascending `BlockId`.
#[derive(Debug, Clone, Default)]
pub struct BlockRegistry {
    defs: BTreeMap<BlockId, BlockDef>,
    by_name: BTreeMap<String, BlockId>,
}

impl BlockRegistry {
    pub fn from_ron(src: &str) -> Result<Self, RegistryError> {
        let manifest: BlockManifest =
            ron::from_str(src).map_err(|e| RegistryError::Parse(e.to_string()))?;

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

        Ok(Self { defs, by_name })
    }

    pub fn id_of(&self, name: &str) -> Option<BlockId> {
        self.by_name.get(name).copied()
    }

    pub fn def(&self, id: BlockId) -> Option<&BlockDef> {
        self.defs.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (BlockId, &BlockDef)> {
        self.defs.iter().map(|(id, def)| (*id, def))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_id_is_reserved_zero() {
        assert_eq!(BlockId::AIR, BlockId(0));
        assert_eq!(BlockId::default(), BlockId::AIR);
    }

    #[test]
    fn parses_shipped_manifest() {
        let reg = BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron"))
            .expect("shipped manifest must parse");
        assert_eq!(reg.id_of("air"), Some(BlockId::AIR));
        assert!(reg.id_of("dirt").is_some());
        assert!(reg.id_of("stone").is_some());
        assert_eq!(reg.def(BlockId::AIR).unwrap().name, "air");
    }

    #[test]
    fn iter_is_ascending_by_id() {
        let reg = BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
        let ids: Vec<u16> = reg.iter().map(|(id, _)| id.0).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "iter must yield ascending ids");
    }

    #[test]
    fn parses_per_face_textures() {
        let src = r#"(blocks:[
            (id:0,name:"air",textures:Untextured),
            (id:1,name:"grass",textures:Faces(top:"t.png",side:"s.png",bottom:"b.png")),
        ])"#;
        let reg = BlockRegistry::from_ron(src).unwrap();
        let g = reg.def(reg.id_of("grass").unwrap()).unwrap();
        assert_eq!(
            g.textures,
            TextureSpec::Faces { top: "t.png".into(), side: "s.png".into(), bottom: "b.png".into() }
        );
    }

    #[test]
    fn rejects_duplicate_id() {
        let src = r#"(blocks:[(id:0,name:"air",textures:Untextured),(id:0,name:"dup",textures:Untextured)])"#;
        assert!(matches!(BlockRegistry::from_ron(src), Err(RegistryError::DuplicateId(0))));
    }

    #[test]
    fn rejects_duplicate_name() {
        let src = r#"(blocks:[(id:0,name:"air",textures:Untextured),(id:1,name:"air",textures:All("x.png"))])"#;
        assert!(matches!(BlockRegistry::from_ron(src), Err(RegistryError::DuplicateName(_))));
    }

    #[test]
    fn requires_air_at_zero() {
        let src = r#"(blocks:[(id:1,name:"dirt",textures:All("dirt.png"))])"#;
        assert!(matches!(BlockRegistry::from_ron(src), Err(RegistryError::MissingAir)));
    }

    #[test]
    fn rejects_misdefined_air() {
        let src = r#"(blocks:[(id:0,name:"stone",textures:All("dirt.png"))])"#;
        assert!(matches!(BlockRegistry::from_ron(src), Err(RegistryError::BadAir)));
    }

    #[test]
    fn rejects_malformed_ron() {
        assert!(matches!(BlockRegistry::from_ron("not ron {"), Err(RegistryError::Parse(_))));
    }
}
