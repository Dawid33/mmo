//! [`MapView`]: one shown map (the minimap, the full map). Each picks the level whose sample is the
//! largest not above its metres per pixel, wants the tiles its rect covers plus one ring (nearest its
//! centre first), and draws each covered tile from the best loaded ancestor (never a hole). The draws
//! are collected here in the main world ([`MapDraws`]) and drawn by `pass.rs` under the Slint layers.

use bevy::math::DVec2;
use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResource;
use game::map_tile::{MAP_TILE_SAMPLES, MapTileKey};
use game::{MapExtent, REGION_BLOCKS};
use worldgen::tile_map::MapLayer;

use super::store::MapTiles;
use crate::renderer::world_map::WorldMapState;

/// The minimap's zoom (metres per window pixel) — and the full map's on opening.
pub const MINIMAP_M_PER_PX: f32 = 1.0;

/// What a view shows: the block surface, or one of the world map's thematic layers (its 1 px per region
/// raster, sampled by world position).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MapLayerKind {
    #[default]
    Surface,
    Theme(MapLayer),
}

#[derive(Component, Clone, Copy, Debug)]
pub struct MapView {
    /// World blocks (x east, z south) at the rect's centre.
    pub centre: DVec2,
    /// Metres (blocks) per window pixel; continuous.
    pub m_per_px: f32,
    /// Window pixels (physical), top-left origin.
    pub rect: Rect,
    pub layer: MapLayerKind,
    pub visible: bool,
}

impl Default for MapView {
    fn default() -> Self {
        MapView {
            centre: DVec2::ZERO,
            m_per_px: 1.0,
            rect: Rect::default(),
            layer: MapLayerKind::Surface,
            visible: false,
        }
    }
}

impl MapView {
    /// The largest level whose sample (2^k m) is not above `m_per_px`, clamped to `0..=top`.
    pub fn level(&self, top: u8) -> u8 {
        let m = self.m_per_px.max(1.0) as f64;
        (m.log2().floor().max(0.0) as u8).min(top)
    }

    pub fn world_to_screen(&self, b: DVec2) -> Vec2 {
        let d = (b - self.centre) / self.m_per_px as f64;
        self.rect.center() + Vec2::new(d.x as f32, d.y as f32)
    }

    pub fn screen_to_world(&self, p: Vec2) -> DVec2 {
        let d = p - self.rect.center();
        self.centre + DVec2::new(d.x as f64, d.y as f64) * self.m_per_px as f64
    }

    /// The keys of level `level` the rect covers, widened by `ring` tiles, nearest the centre first.
    /// Outside `extent` (z off the map, or x off a map that does not wrap) nothing is wanted.
    fn keys(&self, level: u8, ring: i32, extent: Option<MapExtent>) -> Vec<MapTileKey> {
        let (a, b) = (
            self.screen_to_world(self.rect.min),
            self.screen_to_world(self.rect.max),
        );
        let k0 = MapTileKey::containing(level, a.x.floor() as i64, a.y.floor() as i64);
        let k1 = MapTileKey::containing(level, b.x.floor() as i64, b.y.floor() as i64);
        let c = MapTileKey::containing(
            level,
            self.centre.x.floor() as i64,
            self.centre.y.floor() as i64,
        );
        let mut keys: Vec<MapTileKey> = (k0.z - ring..=k1.z + ring)
            .flat_map(|z| (k0.x - ring..=k1.x + ring).map(move |x| MapTileKey { level, x, z }))
            .filter(|k| extent.is_none_or(|e| key_in_extent(*k, e)))
            .collect();
        keys.sort_by_key(|k| {
            (
                (k.x - c.x).abs().max((k.z - c.z).abs()),
                (k.x - c.x).abs() + (k.z - c.z).abs(),
            )
        });
        keys
    }

    /// The tiles to ask for: those the rect covers plus one ring, nearest the centre first.
    pub fn wanted(&self, top: u8, extent: Option<MapExtent>) -> Vec<MapTileKey> {
        self.keys(self.level(top), 1, extent)
    }
}

/// Whether any of `key`'s blocks lies on the map.
pub fn key_in_extent(key: MapTileKey, e: MapExtent) -> bool {
    let rb = REGION_BLOCKS as i64;
    let (ox, oz) = key.origin_block();
    let span = key.span_blocks();
    let (z0, z1) = (
        e.origin.z as i64 * rb,
        (e.origin.z as i64 + e.height as i64) * rb,
    );
    let (x0, x1) = (
        e.origin.x as i64 * rb,
        (e.origin.x as i64 + e.width as i64) * rb,
    );
    oz < z1 && oz + span > z0 && (e.wraps() || (ox < x1 && ox + span > x0))
}

/// One tile quad to draw: where on screen (physical px), which loaded tile's textures, and the
/// sub-rectangle of it (in its samples) that covers the wanted key.
#[derive(Clone, Debug)]
pub struct MapQuad {
    pub screen: Rect,
    pub tile: super::store::GpuTile,
    pub uv: Rect,
    /// Metres per sample of the drawn (possibly ancestor) tile.
    pub sample_m: f32,
    /// The drawn tile's min block (x, z): thematic layers sample the world by position from it.
    pub origin: Vec2,
}

/// One view's draws: its scissor (physical px) and its quads.
#[derive(Clone, Debug)]
pub struct MapViewDraw {
    pub scissor: Rect,
    pub layer: MapLayerKind,
    pub m_per_px: f32,
    pub quads: Vec<MapQuad>,
}

/// Every visible view's draws this frame (main world → render world).
#[derive(Resource, Clone, Default, ExtractResource)]
pub struct MapDraws(pub Vec<MapViewDraw>);

/// The world map's top level (the whole world in one tile), from the session's `WorldMap`.
pub fn top_level(state: &WorldMapState) -> Option<u8> {
    state.map.as_ref().map(game::map_tile::top_level)
}

/// Ask for every visible view's tiles and collect its draws.
pub(super) fn collect_map_draws(
    views: Query<&MapView>,
    state: Res<WorldMapState>,
    mut tiles: ResMut<MapTiles>,
    mut draws: ResMut<MapDraws>,
) {
    draws.0.clear();
    let Some(top) = top_level(&state) else { return };
    let extent = state.map.as_ref().map(|m| m.extent());
    for view in views
        .iter()
        .filter(|v| v.visible && v.rect.width() > 0.0 && v.rect.height() > 0.0)
    {
        tiles.want(view.wanted(top, extent));
        let level = view.level(top);
        let mut quads = Vec::new();
        for key in view.keys(level, 0, extent) {
            let Some((at, gpu)) = tiles.best(key) else {
                continue;
            };
            let (ox, oz) = key.origin_block();
            let span = key.span_blocks() as f64;
            let (ax, az) = at.origin_block();
            let s = at.sample_m() as f64;
            let uv = Rect::new(
                ((ox - ax) as f64 / s) as f32,
                ((oz - az) as f64 / s) as f32,
                ((ox - ax) as f64 / s + span / s) as f32,
                ((oz - az) as f64 / s + span / s) as f32,
            );
            debug_assert!(uv.max.x <= MAP_TILE_SAMPLES as f32 + 0.5);
            let a = view.world_to_screen(DVec2::new(ox as f64, oz as f64));
            let b = view.world_to_screen(DVec2::new(ox as f64 + span, oz as f64 + span));
            quads.push(MapQuad {
                screen: Rect::from_corners(a, b),
                tile: gpu.clone(),
                uv,
                sample_m: s as f32,
                origin: Vec2::new(ax as f32, az as f32),
            });
        }
        draws.0.push(MapViewDraw {
            scissor: view.rect,
            layer: view.layer,
            m_per_px: view.m_per_px,
            quads,
        });
    }
}
