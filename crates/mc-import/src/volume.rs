//! The decoded content of a schematic: a palette and one palette index per cell.

use crate::{ImportError, Tag};
use std::collections::BTreeMap;

/// A block state: `minecraft:oak_stairs` + its properties.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockState {
    pub name: String,
    pub props: BTreeMap<String, String>,
}

impl BlockState {
    /// Parses `"minecraft:oak_stairs[facing=north,half=bottom]"`.
    pub fn parse(s: &str) -> Result<BlockState, ImportError> {
        let bad = || ImportError::NotASchematic(format!("bad block state {s:?}"));
        let (name, rest) = match s.find('[') {
            Some(i) => (&s[..i], Some(&s[i + 1..])),
            None => (s, None),
        };
        if name.is_empty() || name.contains([']', '=', ',']) {
            return Err(bad());
        }
        let mut props = BTreeMap::new();
        if let Some(rest) = rest {
            let inner = rest.strip_suffix(']').ok_or_else(bad)?;
            if !inner.is_empty() {
                for kv in inner.split(',') {
                    let (k, v) = kv.split_once('=').ok_or_else(bad)?;
                    if k.is_empty() || v.contains(['[', ']', '=']) || k.contains(['[', ']']) {
                        return Err(bad());
                    }
                    props.insert(k.to_string(), v.to_string());
                }
            }
        }
        Ok(BlockState {
            name: name.to_string(),
            props,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockEntity {
    pub pos: [i32; 3],
    pub id: String,
    /// The whole entity compound as stored.
    pub nbt: Tag,
}

#[derive(Debug, Clone)]
pub struct BlockVolume {
    /// x (Width), y (Height), z (Length).
    pub size: [u32; 3],
    pub palette: Vec<BlockState>,
    /// Palette index per cell, index = x + z * sx + y * sx * sz.
    pub blocks: Vec<u16>,
    pub block_entities: Vec<BlockEntity>,
    pub data_version: i32,
    /// From Metadata, "" when absent.
    pub name: String,
    pub author: String,
}

impl BlockVolume {
    /// The cell's index into `blocks`. Does not bounds-check.
    pub fn index(&self, x: u32, y: u32, z: u32) -> usize {
        x as usize
            + z as usize * self.size[0] as usize
            + y as usize * self.size[0] as usize * self.size[2] as usize
    }

    /// The block state at a cell; panics when the cell is outside the volume.
    pub fn at(&self, x: u32, y: u32, z: u32) -> &BlockState {
        assert!(
            x < self.size[0] && y < self.size[1] && z < self.size[2],
            "cell outside the volume"
        );
        &self.palette[self.blocks[self.index(x, y, z)] as usize]
    }
}
