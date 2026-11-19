//! The pure water tile mesher: one `WATER_TILE_BLOCKS`² tile of block
//! columns, each a list of inclusive water runs, to blended triangles.
//!
//! - Top faces sit on each run's top cell (the cell above a run is never
//!   water: runs are maximal), greedy-merged over equal top y and equal
//!   alpha; a run with `solid_above` (lake ice) has none.
//! - Side faces face a horizontal neighbour whose cell is neither water nor
//!   solid, spanning the run's height there. Toward an apron side outside
//!   the region (`edge`) there are none: the neighbour region's water is
//!   unknown here, and a wall must never show at a seam.
//! - Colour is [`super::WATER_COLOR`] with the alpha of
//!   [`super::water_alpha`] — the run's depth for a top face, 1 m for a side.
//!
//! Everything is tile-local, in world units (`BLOCK_VOXELS` per block), at
//! the region's block y.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{water_alpha, WATER_COLOR, WATER_TILE_BLOCKS};

/// An inclusive y run of water cells in one block column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaterRun {
    pub y0: i16,
    pub y1: i16,
    /// The cell over `y1` holds a block (ice): no top face.
    pub solid_above: bool,
}

/// One block column of a tile: its water runs (bottom up, maximal) and the
/// inclusive y runs of its solid cells, the latter only over the y span the
/// tile's water can touch (a side face looks no further).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaterColumn {
    pub runs: Vec<WaterRun>,
    pub solid: Vec<(i16, i16)>,
}

impl WaterColumn {
    pub fn is_water(&self, y: i16) -> bool {
        self.runs.iter().any(|r| (r.y0..=r.y1).contains(&y))
    }

    pub fn is_solid(&self, y: i16) -> bool {
        self.solid.iter().any(|&(lo, hi)| (lo..=hi).contains(&y))
    }
}

/// Edge of the tile's column grid with its 1-column apron.
pub const APRON_EDGE: usize = WATER_TILE_BLOCKS as usize + 2;

/// Apron sides, in [`WaterTileInput::edge`] order.
pub const SIDES: [[i32; 2]; 4] = [[-1, 0], [1, 0], [0, -1], [0, 1]];

/// One tile's mesher input: `columns` row major over the tile plus a
/// 1-column apron — `(WATER_TILE_BLOCKS + 2)²` entries, index
/// `(z + 1) * APRON_EDGE + (x + 1)` for tile-local `x, z` in
/// `-1..=WATER_TILE_BLOCKS` (x fastest). `edge[i]` marks apron side
/// [`SIDES`]`[i]` (−x, +x, −z, +z) as outside the region.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaterTileInput {
    /// Region-local block x, z of the tile's first column.
    pub origin: [i32; 2],
    pub columns: Vec<WaterColumn>,
    pub edge: [bool; 4],
}

impl WaterTileInput {
    /// A tile of empty columns.
    pub fn empty(origin: [i32; 2]) -> Self {
        Self {
            origin,
            columns: vec![WaterColumn::default(); APRON_EDGE * APRON_EDGE],
            edge: [false; 4],
        }
    }

    /// Index of tile-local column `(x, z)`, apron included.
    pub fn index(x: i32, z: i32) -> usize {
        (z + 1) as usize * APRON_EDGE + (x + 1) as usize
    }

    pub fn column(&self, x: i32, z: i32) -> &WaterColumn {
        &self.columns[Self::index(x, z)]
    }

    pub fn column_mut(&mut self, x: i32, z: i32) -> &mut WaterColumn {
        &mut self.columns[Self::index(x, z)]
    }
}

/// A meshed tile: triangle-list vertex streams, tile-local world units.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WaterMeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

impl WaterMeshData {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

impl WaterMeshData {
    /// One quad at corner `p` spanned by `u` then `v` (`u × v` is the
    /// normal, so it is front-facing from outside).
    pub(crate) fn quad(&mut self, p: [f32; 3], u: [f32; 3], v: [f32; 3], normal: [f32; 3], alpha: f32) {
        let base = self.positions.len() as u32;
        let add = |a: [f32; 3], b: [f32; 3]| [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
        for corner in [p, add(p, u), add(add(p, u), v), add(p, v)] {
            self.positions.push(corner);
            self.normals.push(normal);
            self.colors
                .push([WATER_COLOR[0], WATER_COLOR[1], WATER_COLOR[2], alpha]);
        }
        self.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// Mesh one tile (see the module doc).
pub fn mesh_water_tile(input: &WaterTileInput) -> WaterMeshData {
    assert_eq!(
        input.columns.len(),
        APRON_EDGE * APRON_EDGE,
        "a tile plus its apron"
    );
    let t = WATER_TILE_BLOCKS;
    let n = t as usize;
    let b = game::BLOCK_VOXELS as f32;
    let mut out = WaterMeshData::default();

    // Top faces, greedy-merged per (top y, alpha).
    let mut tops: BTreeMap<(i16, u32), Vec<bool>> = BTreeMap::new();
    for z in 0..t {
        for x in 0..t {
            for run in input.column(x, z).runs.iter().filter(|r| !r.solid_above) {
                let alpha = water_alpha(f32::from(run.y1 - run.y0 + 1));
                tops.entry((run.y1, alpha.to_bits()))
                    .or_insert_with(|| vec![false; n * n])[x as usize + n * z as usize] = true;
            }
        }
    }
    for ((top, alpha), mut cells) in tops {
        let alpha = f32::from_bits(alpha);
        let y = (f32::from(top) + 1.0) * b;
        for z in 0..n {
            for x in 0..n {
                if !cells[x + n * z] {
                    continue;
                }
                let mut w = 1;
                while x + w < n && cells[x + w + n * z] {
                    w += 1;
                }
                let mut h = 1;
                while z + h < n && (x..x + w).all(|xx| cells[xx + n * (z + h)]) {
                    h += 1;
                }
                for zz in z..z + h {
                    for xx in x..x + w {
                        cells[xx + n * zz] = false;
                    }
                }
                let p = [x as f32 * b, y, z as f32 * b];
                out.quad(
                    p,
                    [0.0, 0.0, h as f32 * b],
                    [w as f32 * b, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    alpha,
                );
            }
        }
    }

    // Side faces toward open (neither water nor solid) neighbour cells.
    let side_alpha = water_alpha(1.0);
    for z in 0..t {
        for x in 0..t {
            let column = input.column(x, z);
            for (side, [dx, dz]) in SIDES.into_iter().enumerate() {
                let (nx, nz) = (x + dx, z + dz);
                let in_apron = nx < 0 || nx >= t || nz < 0 || nz >= t;
                if in_apron && input.edge[side] {
                    continue;
                }
                let next = input.column(nx, nz);
                for run in &column.runs {
                    let mut y = run.y0;
                    while y <= run.y1 {
                        if next.is_water(y) || next.is_solid(y) {
                            y += 1;
                            continue;
                        }
                        let lo = y;
                        while y < run.y1 && !next.is_water(y + 1) && !next.is_solid(y + 1) {
                            y += 1;
                        }
                        side_quad(&mut out, x, z, [dx, dz], lo, y, side_alpha);
                        y += 1;
                    }
                }
            }
        }
    }
    out
}

/// The side face of column `(x, z)` toward `dir`, over cells `lo..=hi`.
fn side_quad(out: &mut WaterMeshData, x: i32, z: i32, dir: [i32; 2], lo: i16, hi: i16, alpha: f32) {
    let b = game::BLOCK_VOXELS as f32;
    let (x0, z0) = (x as f32 * b, z as f32 * b);
    let y0 = f32::from(lo) * b;
    let dy = (f32::from(hi) - f32::from(lo) + 1.0) * b;
    match dir {
        [1, 0] => out.quad(
            [x0 + b, y0, z0],
            [0.0, dy, 0.0],
            [0.0, 0.0, b],
            [1.0, 0.0, 0.0],
            alpha,
        ),
        [-1, 0] => out.quad(
            [x0, y0, z0],
            [0.0, 0.0, b],
            [0.0, dy, 0.0],
            [-1.0, 0.0, 0.0],
            alpha,
        ),
        [0, 1] => out.quad(
            [x0, y0, z0 + b],
            [b, 0.0, 0.0],
            [0.0, dy, 0.0],
            [0.0, 0.0, 1.0],
            alpha,
        ),
        _ => out.quad(
            [x0, y0, z0],
            [0.0, dy, 0.0],
            [b, 0.0, 0.0],
            [0.0, 0.0, -1.0],
            alpha,
        ),
    }
}

/// Mesh one tile off the main thread (a compute job, `renderer::compute`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaterMeshJob(pub WaterTileInput);

impl crate::renderer::compute::ComputeJob for WaterMeshJob {
    type Output = WaterMeshData;
    const KIND: crate::renderer::compute::JobKind = crate::renderer::compute::JobKind::WaterMesh;

    fn run(self) -> WaterMeshData {
        mesh_water_tile(&self.0)
    }
}

