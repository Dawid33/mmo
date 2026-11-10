//! Water as its own blended pass: recipe water is drawn with the blended
//! [`material::WaterMaterial`] (every backend, WebGL2 included), tinted by
//! vertex colour and cut with the working level. The meshes are built by the
//! stud passes — the near surface per chunk column (`stud/water.rs`) and the
//! far tiles' sheets (`stud/far.rs`); this module holds what they share: the
//! material, the tint and alpha rule, and the vertex streams.

pub mod material;
pub mod mesh;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use self::material::{sync_water_material_cut, WaterMaterial, WaterMaterialHandle};
use self::mesh::WaterMeshData;

/// Water tint, linear RGB.
pub const WATER_COLOR: [f32; 3] = [0.10, 0.32, 0.45];
/// Light absorbed per metre of water: alpha = `1 − exp(−this × depth_m)`.
pub const WATER_ABSORPTION_PER_M: f32 = 0.35;
/// Least alpha of a water face (shallow water still reads as water).
pub const WATER_MIN_ALPHA: f32 = 0.35;
/// Most alpha of a water face (the bed of deep water still shows a little).
pub const WATER_MAX_ALPHA: f32 = 0.92;

/// Alpha of water `depth_m` deep (client render code: f32 `exp` is fine).
pub fn water_alpha(depth_m: f32) -> f32 {
    (1.0 - (-WATER_ABSORPTION_PER_M * depth_m).exp()).clamp(WATER_MIN_ALPHA, WATER_MAX_ALPHA)
}

/// Meshed water as a Bevy mesh (`None` when it has no faces).
pub fn tile_mesh(data: WaterMeshData) -> Option<Mesh> {
    if data.is_empty() {
        return None;
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, data.positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, data.normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, data.colors);
    mesh.insert_indices(Indices::U32(data.indices));
    Some(mesh)
}

pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        material::embed_water_shader(app);
        app.add_plugins(MaterialPlugin::<WaterMaterial>::default())
            .init_resource::<WaterMaterialHandle>()
            .add_systems(Update, sync_water_material_cut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_grows_with_depth_between_its_bounds() {
        assert_eq!(water_alpha(0.0), WATER_MIN_ALPHA);
        assert!(water_alpha(2.0) > water_alpha(1.5));
        assert_eq!(water_alpha(1000.0), WATER_MAX_ALPHA);
    }

    #[test]
    fn a_mesh_without_faces_is_no_mesh() {
        assert!(tile_mesh(WaterMeshData::default()).is_none());
        let mut data = WaterMeshData::default();
        data.quad([0.0; 3], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 0.5);
        let mesh = tile_mesh(data).expect("one quad");
        assert_eq!(mesh.count_vertices(), 4);
        assert_eq!(mesh.indices().map(|i| i.len()), Some(6));
    }
}
