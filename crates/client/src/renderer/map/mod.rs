//! The world map (plan `docs/superpowers/plans/2026-11-20-block-level-world-map.md`, spec
//! `2026-11-19-block-level-world-map-design.md`): one tile pyramid behind the minimap, the full map and
//! the site choice. Tiles come from a [`MapSource`] — natively the world's own store
//! ([`MapSource::Local`], `map-tiles` against `terrain.sqlite` opened read-only), else HTTP — into
//! [`MapTiles`], which belongs to the WORLD: a session end keeps it, another world clears it.

pub mod palette;
pub mod store;

use bevy::prelude::*;

pub use palette::MapPalette;
pub use store::{GpuTile, MapSource, MapTiles, MapWorld};

pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        store::build(app);
    }
}
