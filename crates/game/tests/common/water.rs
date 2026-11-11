//! Recipe water for test fixtures: water is the stud ground recipe's
//! `water_top` over its `surface`, never bricks. [`floor_with_water`] builds
//! it into a region's stud genesis (what the seam harness's ground hook and
//! the hand-built fixtures use) — a stud recipe is fixed at genesis, so a
//! fixture states its water before the region is built.
use std::ops::Range;

use game::{ColumnCoords, RegionCoords, StudColumnGround, StudRegionSeed, REGION_BLOCKS, REGION_CHUNKS, STUD_CHUNK_STUDS, TILE_PLATES, TILE_STUDS};

/// Recipe water over the WORLD tile columns `xs × zs`: the ground's top is
/// the top of tile level `surface`, water fills tile levels
/// `surface + 1 ..= water_top`.
#[derive(Debug, Clone)]
pub struct WaterBox {
    pub xs: Range<i64>,
    pub zs: Range<i64>,
    pub surface: i32,
    pub water_top: i32,
}

/// One past the top plate of tile level `level`.
pub fn level_top_plates(level: i32) -> i32 {
    (level + 1) * TILE_PLATES as i32
}

/// Recipe water over the WORLD tile columns `xs × zs`, in plates: the bed's
/// top is `bed` (one past its top solid plate), the water's `top`.
#[derive(Debug, Clone)]
pub struct PlateWater {
    pub xs: Range<i64>,
    pub zs: Range<i64>,
    pub bed: i32,
    pub top: i32,
}

impl From<&WaterBox> for PlateWater {
    fn from(b: &WaterBox) -> Self {
        PlateWater { xs: b.xs.clone(), zs: b.zs.clone(), bed: level_top_plates(b.surface), top: level_top_plates(b.water_top) }
    }
}

/// The chunk column `(cx, cz)` of region `rc`: flat at `dry_plates`, with the
/// recipe water (and beds) of the `boxes` that fall in it.
pub fn column_with_plate_water(rc: RegionCoords, cx: usize, cz: usize, dry_plates: i32, boxes: &[PlateWater]) -> StudColumnGround {
    let (n, s) = (REGION_BLOCKS as i64, STUD_CHUNK_STUDS);
    let mut surface = vec![dry_plates; s * s];
    let mut water = vec![game::STUD_NO_WATER; s * s];
    for sz in 0..s {
        for sx in 0..s {
            let wx = rc.x as i64 * n + ((cx * s + sx) / TILE_STUDS) as i64;
            let wz = rc.z as i64 * n + ((cz * s + sz) / TILE_STUDS) as i64;
            if let Some(b) = boxes.iter().find(|b| b.xs.contains(&wx) && b.zs.contains(&wz)) {
                surface[sz * s + sx] = b.bed;
                water[sz * s + sx] = b.top;
            }
        }
    }
    StudColumnGround::new(
        surface.into(),
        water.into(),
        vec![0; s * s].into(),
        vec![super::SOIL],
        vec![0; s * s].into(),
        vec![(i32::MAX, game::MAT_STONE)],
        super::FLOOR_BEDROCK,
    )
    .expect("a recipe of known materials")
}

/// [`column_with_plate_water`] for tile-level [`WaterBox`]es.
pub fn column_with_water(rc: RegionCoords, cx: usize, cz: usize, dry_plates: i32, boxes: &[WaterBox]) -> StudColumnGround {
    let boxes: Vec<PlateWater> = boxes.iter().map(PlateWater::from).collect();
    column_with_plate_water(rc, cx, cz, dry_plates, &boxes)
}

/// `uniform_floor` (flat stud ground at `common::FLOOR_PLATES` under every
/// chunk column) carrying the recipe water of `boxes` that falls in region `rc`.
pub fn floor_with_water(rc: RegionCoords, boxes: &[WaterBox]) -> StudRegionSeed {
    let ground = (0..REGION_CHUNKS)
        .flat_map(|cx| (0..REGION_CHUNKS).map(move |cz| (cx, cz)))
        .map(|(cx, cz)| (ColumnCoords::new(cx, cz), column_with_water(rc, cx, cz, super::FLOOR_PLATES, boxes)))
        .collect();
    StudRegionSeed { ground, features: Vec::new(), ..Default::default() }
}
