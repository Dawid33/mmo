//! The near window painted locally and at once (spec 5.3): the loaded chunks' tops (`HeightGrid`'s map
//! columns) written into the loaded tiles of levels 0..=[`MAP_PAINT_MAX_LEVEL`] under them, at most every
//! [`MAP_PAINT_EVERY_S`]. A dug hole shows on the minimap within that, while served tiles lag a save. A
//! painted tile outlives its chunks: it is kept until a served tile with a newer etag replaces it (then
//! painted again from whatever is loaded), so nothing un-digs when regions unload or restart elsewhere.

use bevy::prelude::*;
use game::map_tile::{sample_flags, MapTile, MapTileKey, MAP_TILE_SAMPLES};
use game::CHUNK_BLOCKS;

use super::store::MapTiles;
use crate::renderer::commander::height::{HeightGrid, MapColumn};

/// The coarsest level painted locally (8 m per sample): the levels the reader overlays edits on too
/// (`map_tiles::MAP_EDIT_MAX_LEVEL`, which the wasm build cannot name; checked natively below).
pub const MAP_PAINT_MAX_LEVEL: u8 = 3;
#[cfg(not(target_arch = "wasm32"))]
const _: () = assert!(MAP_PAINT_MAX_LEVEL == map_tiles::MAP_EDIT_MAX_LEVEL);
/// The shortest gap between two paints.
pub const MAP_PAINT_EVERY_S: f32 = 0.25;

const EDGE: i64 = MAP_TILE_SAMPLES as i64;

pub(super) fn build(app: &mut App) {
    app.add_systems(Update, paint_near_window);
}

/// Paint what changed: the chunk columns the grid re-derived, and tiles a served answer replaced.
fn paint_near_window(
    grid: Option<ResMut<HeightGrid>>,
    mut tiles: ResMut<MapTiles>,
    mut images: ResMut<Assets<Image>>,
    time: Res<Time>,
    mut since: Local<f32>,
) {
    let Some(mut grid) = grid else { return };
    *since += time.delta_secs();
    if *since < MAP_PAINT_EVERY_S {
        return;
    }
    *since = 0.0;
    let cb = CHUNK_BLOCKS as i64;
    // (key, block rect [x0, z0, x1, z1)) to paint.
    let mut work: Vec<(MapTileKey, [i64; 4])> = Vec::new();
    for (cx, cz) in grid.take_dirty() {
        let (x0, z0) = (cx as i64 * cb, cz as i64 * cb);
        for level in 0..=MAP_PAINT_MAX_LEVEL {
            // A chunk column lies inside one tile at every level (a tile is a whole number of chunks).
            work.push((MapTileKey::containing(level, x0, z0), [x0, z0, x0 + cb, z0 + cb]));
        }
    }
    for key in tiles.take_replaced() {
        let (ox, oz) = key.origin_block();
        let span = key.span_blocks();
        work.push((key, [ox, oz, ox + span, oz + span]));
    }
    for (key, rect) in work {
        tiles.paint(key, &mut images, |tile| paint_rect(tile, rect, |x, z| grid.map_column(x, z)));
    }
}

/// Set every sample of `tile` whose centre block lies in `rect` and has a loaded column; whether any did.
fn paint_rect(tile: &mut MapTile, [x0, z0, x1, z1]: [i64; 4], column: impl Fn(i32, i32) -> Option<MapColumn>) -> bool {
    let key = tile.key;
    let (ox, oz) = key.origin_block();
    let sm = key.sample_m();
    // Sample indices whose centre block `o + i·sm + sm/2` lies in [a, b).
    let range = |o: i64, a: i64, b: i64| {
        let first = (a - o - sm / 2 + sm - 1).div_euclid(sm).max(0);
        let last = (b - 1 - o - sm / 2).div_euclid(sm).min(EDGE - 1);
        first..=last
    };
    let mut changed = false;
    for sz in range(oz, z0, z1) {
        for sx in range(ox, x0, x1) {
            let (bx, bz) = (ox + sx * sm + sm / 2, oz + sz * sm + sm / 2);
            let Some(c) = column(bx as i32, bz as i32) else { continue };
            let s = &mut tile.samples[(sx + EDGE * sz) as usize];
            let water = c.water_top.map_or(0, |w| (i32::from(w) + 1 - i32::from(c.height_m)).clamp(0, u8::MAX as i32) as u8);
            let ice = s.flags & sample_flags::ICE;
            let painted = game::map_tile::MapSample {
                height: c.height_m,
                block: c.block,
                water,
                flags: sample_flags::REAL | if water > 0 { ice } else { 0 },
            };
            if *s != painted {
                *s = painted;
                changed = true;
            }
        }
    }
    changed
}
