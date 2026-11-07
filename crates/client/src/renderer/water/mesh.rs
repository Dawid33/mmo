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
    fn quad(&mut self, p: [f32; 3], u: [f32; 3], v: [f32; 3], normal: [f32; 3], alpha: f32) {
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

#[cfg(test)]
mod tests {
    use super::super::{WATER_MAX_ALPHA, WATER_MIN_ALPHA};
    use super::*;

    const T: i32 = WATER_TILE_BLOCKS;

    /// A tile whose every column (apron included) is solid ground up to
    /// `ground` and has no water.
    fn grounded(ground: i16) -> WaterTileInput {
        let mut input = WaterTileInput::empty([0, 0]);
        for c in &mut input.columns {
            c.solid = vec![(i16::MIN, ground)];
        }
        input
    }

    fn pond(
        input: &mut WaterTileInput,
        xs: std::ops::Range<i32>,
        zs: std::ops::Range<i32>,
        y0: i16,
        y1: i16,
    ) {
        for x in xs {
            for z in zs.clone() {
                let c = input.column_mut(x, z);
                c.solid = vec![(i16::MIN, y0 - 1)];
                c.runs = vec![WaterRun {
                    y0,
                    y1,
                    solid_above: false,
                }];
            }
        }
    }

    fn faces(mesh: &WaterMeshData, normal: [f32; 3]) -> Vec<usize> {
        // Quads are 4 vertices with one normal; return the first vertex of each.
        (0..mesh.positions.len())
            .step_by(4)
            .filter(|&v| mesh.normals[v] == normal)
            .collect()
    }

    fn top_alpha(mesh: &WaterMeshData) -> f32 {
        let v = faces(mesh, [0.0, 1.0, 0.0])[0];
        mesh.colors[v][3]
    }

    #[test]
    fn flat_pond_is_one_top_quad() {
        let mut input = grounded(9);
        pond(&mut input, 4..8, 4..8, 7, 9);
        let mesh = mesh_water_tile(&input);
        let tops = faces(&mesh, [0.0, 1.0, 0.0]);
        assert_eq!(tops.len(), 1, "one merged top quad");
        assert_eq!(mesh.positions.len(), 4, "only the top: 4 vertices");
        assert_eq!(mesh.indices.len(), 6);
        let b = game::BLOCK_VOXELS as f32;
        assert!(
            mesh.positions.iter().all(|p| p[1] == 10.0 * b),
            "the top lies on the run's top cell"
        );
    }

    #[test]
    fn no_top_face_under_ice() {
        let mut input = grounded(9);
        pond(&mut input, 4..8, 4..8, 7, 9);
        for x in 4..8 {
            for z in 4..8 {
                let c = input.column_mut(x, z);
                c.runs[0].solid_above = true;
                c.solid.push((10, 10));
            }
        }
        let mesh = mesh_water_tile(&input);
        assert!(
            faces(&mesh, [0.0, 1.0, 0.0]).is_empty(),
            "no top face under ice"
        );
    }

    #[test]
    fn side_face_toward_a_dry_hole() {
        let mut input = grounded(9);
        pond(&mut input, 4..5, 4..5, 7, 9);
        // A dug hole east of the pond: no water, no solid over the run.
        input.column_mut(5, 4).solid = vec![(i16::MIN, 4)];
        let mesh = mesh_water_tile(&input);
        let east = faces(&mesh, [1.0, 0.0, 0.0]);
        assert_eq!(east.len(), 1, "one side face toward the hole");
        let b = game::BLOCK_VOXELS as f32;
        let ys: Vec<f32> = (0..4).map(|i| mesh.positions[east[0] + i][1]).collect();
        let (lo, hi) = ys
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), &y| (lo.min(y), hi.max(y)));
        assert_eq!((lo, hi), (7.0 * b, 10.0 * b), "spans the run");
        assert!(mesh.positions[east[0]..east[0] + 4]
            .iter()
            .all(|p| p[0] == 5.0 * b));
        assert_eq!(
            mesh.normals.iter().filter(|n| n[1] == 0.0).count(),
            4,
            "no other side face"
        );
    }

    #[test]
    fn no_side_faces_on_region_edge() {
        let mut input = WaterTileInput::empty([0, 0]);
        // A pond filling the tile's −x column; the apron west of it is the
        // region edge (no data there: neither water nor solid).
        for z in 0..T {
            for x in -1..=T {
                input.column_mut(x, z).solid = vec![(i16::MIN, 9)];
            }
        }
        pond(&mut input, 0..1, 0..T, 7, 9);
        input.column_mut(-1, 0).solid.clear();
        input.edge[0] = true;
        let mesh = mesh_water_tile(&input);
        assert!(
            faces(&mesh, [-1.0, 0.0, 0.0]).is_empty(),
            "no wall toward the region edge"
        );

        input.edge[0] = false;
        let mesh = mesh_water_tile(&input);
        assert_eq!(
            faces(&mesh, [-1.0, 0.0, 0.0]).len(),
            1,
            "inside the region the same hole shows a side"
        );
    }

    #[test]
    fn deeper_water_is_more_opaque() {
        let mut shallow = grounded(0);
        pond(&mut shallow, 4..8, 4..8, 1, 1);
        let mut deep = grounded(0);
        pond(&mut deep, 4..8, 4..8, 1, 10);
        let (a1, a10) = (
            top_alpha(&mesh_water_tile(&shallow)),
            top_alpha(&mesh_water_tile(&deep)),
        );
        assert!(a10 > a1, "{a10} > {a1}");
        for a in [a1, a10] {
            assert!((WATER_MIN_ALPHA..=WATER_MAX_ALPHA).contains(&a), "{a}");
        }
    }
}
