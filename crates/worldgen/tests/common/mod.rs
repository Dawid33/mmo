//! The small synthetic forged world shared by the forged-world test binaries:
//! a western sea, a plain rising east, a lake basin and one river, whose
//! right answers are known.
#![allow(dead_code)] // each test binary uses its own subset

use game::BlockId;
use worldgen::forged::{GEO_STEP_M, HEIGHT_STEP_M, Lake, QRaster, PRECIP_STEP_MM, TEMPERATURE_STEP_C, RiverPoint, RiverReach, WorldArtifact, ARTIFACT_VERSION};

pub const N: usize = 64;
pub const CELL_M: f64 = 16.0;
pub const SEA_LEVEL: f64 = 64.0;
pub const SEA_TOP_BLOCK: i32 = 63;
/// River along this artifact z, flowing west to the sea.
pub const RIVER_Z: f64 = 512.0;
pub const RIVER_WIDTH_M: f32 = 8.0;
/// Lake cells (artifact cell indices) and its level (m above sea).
pub const LAKE_CELLS: (std::ops::Range<usize>, std::ops::Range<usize>) = (30..36, 10..16);
pub const LAKE_LEVEL: f32 = 16.6;
pub const SPAWN: [f64; 2] = [512.0, 600.0];
/// The synthetic geology: indices into `ROCKS` (= `rock_names`).
pub const SANDSTONE: u8 = 0;
pub const SHALE: u8 = 1;

/// A plain rising eastward out of a western sea: h = 2 + 0.03 x.
pub fn plain(ax: f64) -> f64 {
    2.0 + 0.03 * ax
}

pub fn mats() -> worldgen::material::Materials {
    worldgen::material::Materials {
        stone: BlockId(2),
        dirt: BlockId(1),
        grass: BlockId(3),
        sand: BlockId(4),
        water: BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
        trees: worldgen::material::TreeBlocks::NONE,
    }
}

pub fn synthetic() -> WorldArtifact {
    let mut height = vec![0f32; N * N];
    let mut lake_cells = Vec::new();
    for iz in 0..N {
        for ix in 0..N {
            let ax = (ix as f64 + 0.5) * CELL_M;
            let i = iz * N + ix;
            height[i] = if ix < 4 { -5.0 } else { plain(ax) as f32 };
            if LAKE_CELLS.0.contains(&ix) && LAKE_CELLS.1.contains(&iz) {
                height[i] -= 5.0; // the basin
                lake_cells.push((i as u32, 1));
            }
        }
    }
    // Source in the east, mouth in the western sea; water just under the plain.
    let points = (4..=56)
        .rev()
        .map(|k| {
            let x = k as f64 * CELL_M;
            let water = (plain(x) - 0.2) as f32;
            RiverPoint { x: x as f32, z: RIVER_Z as f32, bed: water - 1.0, water, in_lake: false }
        })
        .collect();
    WorldArtifact {
        version: ARTIFACT_VERSION,
        seed: 7,
        size_m: N as f64 * CELL_M,
        cell_m: CELL_M,
        n: N as u32,
        height: QRaster::fit(&height, HEIGHT_STEP_M),
        uplifted: QRaster::fit(&vec![0.0; N * N], GEO_STEP_M),
        bedding: QRaster::fit(&vec![0.0; N * N], GEO_STEP_M),
        temperature_c: QRaster::fit(&vec![14.0; N * N], TEMPERATURE_STEP_C),
        precip_mm: QRaster::fit(&vec![900.0; N * N], PRECIP_STEP_MM),
        column: vec![(-5.0, SANDSTONE), (-25.0, SHALE)],
        rock_names: worldgen::forge::geology::ROCKS.iter().map(|r| r.name.to_string()).collect(),
        rivers: vec![RiverReach { order: 1, discharge_m2: 2.0e6, width_m: RIVER_WIDTH_M, points }],
        lakes: vec![Lake { level: LAKE_LEVEL, area_m2: 36.0 * 256.0, max_depth: 5.0, outlet: [480.0, 200.0], cells: lake_cells }],
        spawn: SPAWN,
        edges: [worldgen::forged::EdgeKind::Sea; 4],
    }
}

/// Artifact meters -> engine world meters (spawn lands on engine (8.5, 8.5)).
pub fn engine(ax: f64, az: f64) -> (f64, f64) {
    (ax - SPAWN[0] + 8.5, az - SPAWN[1] + 8.5)
}
