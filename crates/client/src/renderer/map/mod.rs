//! The world map (plan `docs/superpowers/plans/2026-11-20-block-level-world-map.md`, spec
//! `2026-11-19-block-level-world-map-design.md`): one tile pyramid behind the minimap, the full map and
//! the site choice. Tiles come from a [`MapSource`] — natively the world's own store
//! ([`MapSource::Local`], `map-tiles` against `terrain.sqlite` opened read-only), else HTTP — into
//! [`MapTiles`], which belongs to the WORLD: a session end keeps it, another world clears it.

pub mod full;
pub mod palette;
pub mod pass;
pub mod store;
pub mod views;

use bevy::prelude::*;

pub use palette::MapPalette;
pub use store::{GpuTile, MapSource, MapTiles, MapWorld};
pub use views::{MapLayerKind, MapView};

pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        store::build(app);
        pass::build(app);
        full::build(app);
        app.add_systems(PostUpdate, views::collect_map_draws);
    }

    fn finish(&self, app: &mut App) {
        pass::finish(app);
    }
}
