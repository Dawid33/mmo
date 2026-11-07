//! The water material: Bevy's `StandardMaterial` (blended, glossy, tinted by
//! the mesh's vertex colours) extended with the z-level cut, so water above
//! the working level is hidden in the home region like the terrain.

use bevy::{
    mesh::MeshVertexBufferLayoutRef,
    pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline},
    prelude::*,
    reflect::TypePath,
    render::render_resource::{
        AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
    },
    shader::ShaderRef,
};

use super::super::fortress_view::FortressViewUniform;

/// The material water tiles (and later LOD water) draw with.
pub type WaterMaterial = ExtendedMaterial<StandardMaterial, WaterExtension>;

/// Perceptual roughness of the water surface.
pub const WATER_ROUGHNESS: f32 = 0.15;
/// Specular reflectance of the water surface.
pub const WATER_REFLECTANCE: f32 = 0.5;

/// `assets/shaders/water.wgsl`, compiled in (no hot reload; see
/// `voxel_material::VOXEL_TEXTURE_SHADER` for why shaders are embedded).
pub const WATER_SHADER: Handle<Shader> =
    bevy::asset::uuid_handle!("a3c1e9d2-4b7f-4e60-8d15-6f2b0c9e7a41");

/// Register [`WATER_SHADER`] (before the material is used).
pub fn embed_water_shader(app: &mut App) {
    bevy::asset::load_internal_asset!(
        app,
        WATER_SHADER,
        "../../../../../assets/shaders/water.wgsl",
        Shader::from_wgsl
    );
}

/// The cut, as `water.wgsl` reads it at binding 100 (kept in step by
/// [`sync_water_material_cut`]).
#[derive(Asset, AsBindGroup, Debug, Clone, TypePath)]
pub struct WaterExtension {
    #[uniform(100)]
    pub fortress: FortressViewUniform,
}

/// A fresh water material: white base (the vertex colour carries the tint
/// and the depth alpha), blended, double-sided.
pub fn water_material(fortress: FortressViewUniform) -> WaterMaterial {
    ExtendedMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: WATER_ROUGHNESS,
            reflectance: WATER_REFLECTANCE,
            double_sided: true,
            cull_mode: None,
            ..default()
        },
        extension: WaterExtension { fortress },
    }
}

impl MaterialExtension for WaterExtension {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(WATER_SHADER)
    }

    // Blended and never a shadow caster: no depth prepass, no shadow pass.
    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Top faces are seen from below too; one blended layer writes depth
        // (§B.3's single-layer rule: the nearest water surface wins).
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(true);
        }
        Ok(())
    }
}

/// The shared water material handle (made on first use).
#[derive(Resource, Default)]
pub struct WaterMaterialHandle(pub Option<Handle<WaterMaterial>>);

impl WaterMaterialHandle {
    /// The handle, adding the material on first call.
    pub fn get_or_add(
        &mut self,
        materials: &mut Assets<WaterMaterial>,
        fortress: FortressViewUniform,
    ) -> Handle<WaterMaterial> {
        self.0
            .get_or_insert_with(|| materials.add(water_material(fortress)))
            .clone()
    }
}

/// Copy the fortress view into the water material when it changes (as
/// `voxel_material::sync_chunk_material_cut` does for the per-chunk terrain).
pub fn sync_water_material_cut(
    uniform: Option<Res<FortressViewUniform>>,
    handle: Res<WaterMaterialHandle>,
    materials: Option<ResMut<Assets<WaterMaterial>>>,
) {
    let (Some(uniform), Some(handle), Some(mut materials)) =
        (uniform, handle.0.as_ref(), materials)
    else {
        return;
    };
    if materials
        .get(handle)
        .is_some_and(|m| m.extension.fortress != *uniform)
    {
        if let Some(mut m) = materials.get_mut(handle) {
            m.extension.fortress = *uniform;
        }
    }
}
