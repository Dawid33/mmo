//! Laying a resolved prefab out in the world, one chunk at a time. Worldgen and runtime stamping both
//! call [`layout_chunk`]; a chunk reads only its own share of the prefab, never the whole of it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::file::Marker;
use super::resolve::{Cell, ResolvedPrefab, ResolvedShape};
use super::turn::{turn_voxel, turn_xz};
use super::Shape;
use crate::registry::BlockId;
use crate::{Block, BlockIndex, ChiselData, BLOCK_VOXELS, CHUNK_BLOCKS};

const CB: i32 = CHUNK_BLOCKS as i32;

/// Where a prefab goes: its anchor cell lands on world block `anchor`. Mirror (x -> -x about the anchor)
/// is applied first, then `turn` quarter turns clockwise seen from above (`turn_xz`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, state_hash::StateHash,
)]
pub struct Placement {
    pub anchor: [i32; 3],
    pub turn: u8,
    pub mirror: bool,
}

/// The writes of one chunk: `carve` cells become air; `writes` cells take the block (and chisel).
/// Both ascending by `BlockIndex`; no cell is in both.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChunkStamp {
    pub carve: Vec<BlockIndex>,
    pub writes: Vec<(BlockIndex, Block, Option<ChiselData>)>,
}

pub fn world_of_cell(p: &ResolvedPrefab, at: &Placement, cell: [i32; 3]) -> [i32; 3] {
    let mut x = cell[0] - p.anchor[0];
    let y = cell[1] - p.anchor[1];
    let z = cell[2] - p.anchor[2];
    if at.mirror {
        x = -x;
    }
    let (x, z) = turn_xz(x, z, at.turn);
    [at.anchor[0] + x, at.anchor[1] + y, at.anchor[2] + z]
}

pub fn cell_of_world(p: &ResolvedPrefab, at: &Placement, world: [i32; 3]) -> [i32; 3] {
    let (x, z) = turn_xz(world[0] - at.anchor[0], world[2] - at.anchor[2], (4 - at.turn % 4) % 4);
    let x = if at.mirror { -x } else { x };
    [x + p.anchor[0], world[1] - at.anchor[1] + p.anchor[1], z + p.anchor[2]]
}

/// World-block bounds of the whole prefab box: (min inclusive, max exclusive).
pub fn placement_bounds(p: &ResolvedPrefab, at: &Placement) -> ([i32; 3], [i32; 3]) {
    let last = [0, 1, 2].map(|a| p.size[a] as i32 - 1);
    let a = world_of_cell(p, at, [0, 0, 0]);
    let b = world_of_cell(p, at, last);
    ([0, 1, 2].map(|i| a[i].min(b[i])), [0, 1, 2].map(|i| a[i].max(b[i]) + 1))
}

/// World chunk coordinates the bounds overlap, ascending.
pub fn chunks_touched(p: &ResolvedPrefab, at: &Placement) -> Vec<[i32; 3]> {
    let (min, max) = placement_bounds(p, at);
    let lo = min.map(|v| v.div_euclid(CB));
    let hi = max.map(|v| (v - 1).div_euclid(CB));
    let mut out = Vec::new();
    for x in lo[0]..=hi[0] {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                out.push([x, y, z]);
            }
        }
    }
    out
}

fn voxel_chisel(cuboids: &[([u8; 3], [u8; 3], u8)], mats: &[BlockId], at: &Placement) -> ChiselData {
    let n = BLOCK_VOXELS;
    let mut table: Vec<Option<u8>> = vec![None; n * n * n];
    for (lo, hi, m) in cuboids {
        for z in lo[2] as usize..(hi[2] as usize).min(n) {
            for y in lo[1] as usize..(hi[1] as usize).min(n) {
                for x in lo[0] as usize..(hi[0] as usize).min(n) {
                    let mx = if at.mirror { n - 1 - x } else { x };
                    let d = turn_voxel([mx, y, z], at.turn);
                    table[d[0] + d[1] * n + d[2] * n * n] = Some(*m);
                }
            }
        }
    }
    ChiselData::from_fn(mats.to_vec(), |x, y, z| table[x + y * n + z * n * n])
}

type Write = (Block, Option<ChiselData>);

/// None when the chunk gets nothing.
pub fn layout_chunk(p: &ResolvedPrefab, at: &Placement, chunk: [i32; 3]) -> Option<ChunkStamp> {
    let wmin = chunk.map(|c| c * CB);
    let wmax = wmin.map(|v| v + CB - 1);
    // Inclusive corners, re-normalised: a mirror or turn swaps which corner is the minimum.
    let a = cell_of_world(p, at, wmin);
    let b = cell_of_world(p, at, wmax);
    let cmin = [0, 1, 2].map(|i| a[i].min(b[i]));
    let cmax = [0, 1, 2].map(|i| a[i].max(b[i]) + 1);

    let mut chisels: BTreeMap<(Shape, BlockId), ChiselData> = BTreeMap::new();
    let mut cells: BTreeMap<BlockIndex, Option<Write>> = BTreeMap::new();
    p.for_each_cell(cmin, cmax, &mut |cell, c| {
        let w = world_of_cell(p, at, cell);
        let [x, y, z] = [0, 1, 2].map(|i| (w[i] - wmin[i]) as usize);
        debug_assert!(x < CHUNK_BLOCKS && y < CHUNK_BLOCKS && z < CHUNK_BLOCKS);
        let entry: Option<Write> = match c {
            Cell::Keep => return,
            Cell::Carve => None,
            Cell::Block(id) => Some((Block { id, data: [0; 3] }, None)),
            Cell::Shaped(ResolvedShape::Named { shape, mat }) => {
                let s = if at.mirror { shape.mirrored_x() } else { *shape }.turned(at.turn);
                let chisel = chisels.entry((s, *mat)).or_insert_with(|| s.chisel(*mat)).clone();
                Some((Block { id: *mat, data: [0; 3] }, Some(chisel)))
            }
            Cell::Shaped(ResolvedShape::Cuboids { cuboids, mats }) => {
                let id = mats.first().copied().unwrap_or_default();
                Some((Block { id, data: [0; 3] }, Some(voxel_chisel(cuboids, mats, at))))
            }
        };
        cells.insert(BlockIndex::from_xyz(x, y, z), entry);
    });
    if cells.is_empty() {
        return None;
    }
    let mut stamp = ChunkStamp::default();
    for (i, e) in cells {
        match e {
            None => stamp.carve.push(i),
            Some((b, c)) => stamp.writes.push((i, b, c)),
        }
    }
    Some(stamp)
}

/// A marker's world block and its turn after the placement (a mirror flips east/west turns: 1 <-> 3).
pub fn marker_in_world(p: &ResolvedPrefab, at: &Placement, marker: &Marker) -> ([i32; 3], u8) {
    let t = marker.turn % 4;
    let t = if at.mirror { (4 - t) % 4 } else { t };
    (world_of_cell(p, at, marker.at), (t + at.turn % 4) % 4)
}
