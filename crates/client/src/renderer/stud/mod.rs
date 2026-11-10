//! Client mirror of stud chunks (the Lego-stud world's building unit) and of
//! the region's player body size.
//!
//! `bridge.rs` spawns one entity per sim stud chunk (from a region snapshot or
//! an `AddStudChunk` update) carrying [`SimStudChunk`]; `SetStudChunk` rewrites
//! its `chunk` in place, so `Changed<SimStudChunk>` is the trigger for the
//! stud index (`world::StudWorld`, which schedules the off-thread mesher), the
//! tile mirror and the height grid.

use bevy::prelude::*;

use crate::session::{world_systems_enabled, SessionResetExt};

pub mod bodies;
pub mod build;
pub mod far;
pub mod material;
#[cfg(test)]
mod measure;
pub mod mesh;
pub mod overlay;
pub mod palette;
pub mod palette_ui;
pub mod systems;
pub mod water;
pub mod world;

/// The sim's stud chunk as the client last saw it. The entity also carries
/// `SimEntity` (region + key), sits under the region's terrain root, and is
/// skipped by `interpolate_transforms` (stud chunks never move).
#[derive(Component, Clone)]
pub struct SimStudChunk {
    pub coords: game::StudChunkCoords,
    pub chunk: game::StudChunk,
}

/// The player body's size in the region(s) being played (`GameData::body_scale`),
/// set from region snapshots. All regions of one world share one scale.
#[derive(Resource, Clone, Copy)]
pub struct ClientBodyScale(pub game::movement::BodyScale);

impl Default for ClientBodyScale {
    /// A stud world's body, until the first snapshot says so itself.
    fn default() -> Self {
        Self(game::movement::BodyScale::STUD)
    }
}

pub struct StudPlugin;

impl Plugin for StudPlugin {
    fn build(&self, app: &mut App) {
        material::embed_stud_terrain_shader(app);
        app.add_plugins(bevy::pbr::MaterialPlugin::<material::StudTerrainMaterial>::default())
            .add_session_reset::<ClientBodyScale>()
            .add_session_reset::<material::StudMaterials>()
            // After the fortress view is rebuilt this frame (the commander
            // plugin's `update_fortress_view`).
            .add_systems(
                Update,
                (material::sync_stud_materials, material::retarget_stud_materials)
                    .after(crate::renderer::fortress_view::update_fortress_view)
                    .run_if(world_systems_enabled()),
            )
            .add_session_reset::<build::StudBuild>()
            .add_session_reset::<build::StudTarget>()
            .add_session_reset::<world::StudWorld>()
            .add_session_reset::<systems::StudMeshTasks>()
            .add_message::<world::StudWorldMsg>()
            .add_session_message_clear::<world::StudWorldMsg>()
            .add_systems(
                Update,
                (
                    palette_ui::apply_stud_ui_actions,
                    build::build_keys,
                    build::build_pick,
                    build::build_apply.after(crate::renderer::commander::hold_pointer_while_orbiting),
                    palette_ui::sync_stud_ui,
                )
                    .chain()
                    .run_if(world_systems_enabled()),
            )
            .add_systems(
                Update,
                (systems::track_stud_world, systems::stud_detail, systems::submit_stud_meshes, systems::apply_stud_meshes)
                    .chain()
                    .run_if(world_systems_enabled()),
            )
            .add_session_reset::<far::StudLodTasks>()
            .add_systems(Update, far::apply_stud_lod_meshes.run_if(world_systems_enabled()))
            .add_session_reset::<water::StudWaterBuilt>()
            .init_resource::<crate::renderer::water::material::WaterMaterialHandle>()
            .add_systems(
                Update,
                (water::build_stud_water.after(systems::track_stud_world), overlay::update_stud_overlay)
                    .run_if(world_systems_enabled()),
            )
            .add_systems(
                Update,
                (bodies::spawn_stud_body_meshes, bodies::apply_stud_body_meshes).chain().run_if(world_systems_enabled()),
            );
    }
}
