//! Test helpers: a block volume from a closure.
use mc_import::{BlockState, BlockVolume};
use std::collections::BTreeMap;

/// A volume from a size and a closure naming each cell's state, building the palette as it goes.
pub fn vol(size: [u32; 3], mut f: impl FnMut(u32, u32, u32) -> String) -> BlockVolume {
    let mut palette: Vec<BlockState> = Vec::new();
    let mut seen: BTreeMap<String, u16> = BTreeMap::new();
    let mut blocks = Vec::new();
    for y in 0..size[1] {
        for z in 0..size[2] {
            for x in 0..size[0] {
                let s = f(x, y, z);
                let id = *seen.entry(s.clone()).or_insert_with(|| {
                    palette.push(BlockState::parse(&s).unwrap());
                    (palette.len() - 1) as u16
                });
                blocks.push(id);
            }
        }
    }
    BlockVolume {
        size,
        palette,
        blocks,
        block_entities: vec![],
        data_version: 3463,
        name: String::new(),
        author: String::new(),
    }
}
