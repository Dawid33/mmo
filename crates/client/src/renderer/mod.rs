use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::{ExtendedMaterial, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};
use crossbeam::channel::{Receiver, Sender};
use game::{ClientId, ClientUpdateEvent, GameEventKind};
use std::collections::BTreeMap;

mod bridge;
pub mod convert;
mod input;
mod interpolate;
pub mod meshing;
mod voxel_material;
pub use bridge::*;
use voxel_material::StandardVoxelMaterial;

#[derive(Resource)]
pub struct ClientUpdates(pub Receiver<ClientUpdateEvent>);

#[derive(Resource)]
pub struct GameEvents(pub Sender<GameEventKind>);

#[derive(Resource, Default)]
pub struct LocalPlayer(pub Option<ClientId>);

/// Maps a simulation `VoxelType` to the array-texture layer index that renders it.
#[derive(Resource, Default, Clone)]
pub struct VoxelTypeLayers(pub BTreeMap<rollback::VoxelType, u32>);

pub struct SimBridgePlugin {
    pub client_recv: Receiver<ClientUpdateEvent>,
    pub game_send: Sender<GameEventKind>,
}

impl Plugin for SimBridgePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>::default())
            .insert_resource(ClientUpdates(self.client_recv.clone()))
            .insert_resource(GameEvents(self.game_send.clone()))
            .init_resource::<LocalPlayer>()
            .init_resource::<bridge::Regions>()
            .init_resource::<bridge::RegionRoots>()
            .init_resource::<bridge::SimEntityMap>()
            .add_systems(
                PreUpdate,
                (
                    input::forward_input,
                    (bridge::drain_client_updates, bridge::drain_region_updates).chain(),
                )
                    .chain(),
            )
            .add_systems(Startup, setup_scene)
            .add_systems(
                Update,
                (meshing::queue_meshing, meshing::apply_meshed_chunks, interpolate::interpolate_transforms),
            );
    }
}

/// Resolve the blocks asset directory; must mirror AssetPlugin.file_path resolution in main.rs.
fn resolve_blocks_dir() -> std::path::PathBuf {
    use std::path::PathBuf;

    // Try CARGO_MANIFEST_DIR (set by cargo at runtime); mirrors BeVy's AssetPlugin behavior.
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        return PathBuf::from(manifest_dir).join("../../assets/blocks");
    }

    // Fall back to exe directory if running outside cargo.
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            return exe_dir.join("assets/blocks");
        }
    }

    // Final fallback: cwd-relative (original behavior).
    PathBuf::from("assets/blocks")
}

fn setup_scene(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut voxel_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>>,
) {
    let mut layers: Vec<image::RgbaImage> = Vec::new();
    let mut layer_names: Vec<String> = Vec::new();
    let mut sorted: BTreeMap<String, image::RgbaImage> = BTreeMap::new();
    let blocks_dir = resolve_blocks_dir();
    if let Ok(dir) = std::fs::read_dir(&blocks_dir) {
        for file in dir {
            let file = match file {
                Ok(file) => file,
                Err(_) => continue,
            };
            if file.path().extension().and_then(|e| e.to_str()) != Some("png") {
                continue;
            }
            match image::ImageReader::open(file.path()) {
                Ok(reader) => match reader.decode() {
                    Ok(decoded) => {
                        sorted.insert(file.file_name().to_string_lossy().to_string(), decoded.to_rgba8());
                    }
                    Err(e) => warn!("failed to decode {:?}, {:?}", file.path(), e),
                },
                Err(e) => warn!("failed to read {:?}, {:?}", file.path(), e),
            }
        }
    }
    for (name, image) in sorted {
        layer_names.push(name);
        layers.push(image);
    }

    let mut voxel_type_layers = VoxelTypeLayers::default();
    let handle = if layers.is_empty() {
        warn!("no block textures found under assets/blocks; voxels will render untextured");
        None
    } else {
        let (w, h) = (layers[0].width(), layers[0].height());
        assert!(layers.iter().all(|l| l.dimensions() == (w, h)), "all block textures must share dimensions");
        let data: Vec<u8> = layers.iter().flat_map(|l| l.as_raw().clone()).collect();
        let mut array_image = Image::new(
            Extent3d { width: w, height: h, depth_or_array_layers: layers.len() as u32 },
            TextureDimension::D2,
            data,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        );
        // `ImageSampler::nearest()` alone leaves clamp-to-edge addressing, which breaks the
        // tiled UVs the mesher emits (uv * quad.width/height can exceed 1.0); force repeat
        // addressing on top of nearest filtering.
        let mut sampler = ImageSamplerDescriptor::nearest();
        sampler.address_mode_u = ImageAddressMode::Repeat;
        sampler.address_mode_v = ImageAddressMode::Repeat;
        sampler.address_mode_w = ImageAddressMode::Repeat;
        array_image.sampler = ImageSampler::Descriptor(sampler);
        // wgpu's default texture-view dimension for a `D2` texture collapses to plain `D2`
        // when `depth_or_array_layers == 1` (per the WebGPU default-view algorithm), which
        // mismatches the `dimension = "2d_array"` binding `StandardVoxelMaterial` declares as
        // soon as there's only a single block texture (e.g. just `dirt.png` today). Force the
        // view to stay an array so the bind group is valid regardless of layer count.
        array_image.texture_view_descriptor = Some(TextureViewDescriptor {
            dimension: Some(TextureViewDimension::D2Array),
            ..Default::default()
        });
        Some(images.add(array_image))
    };

    if let Some(handle) = handle {
        let black_layer = layer_names.iter().position(|n| n == "black.png").unwrap_or(0) as u32;
        voxel_type_layers.0.insert(rollback::VoxelType::Black, black_layer);

        commands.insert_resource(meshing::ChunkMaterial(voxel_materials.add(ExtendedMaterial {
            base: StandardMaterial { perceptual_roughness: 0.9, ..Default::default() },
            extension: StandardVoxelMaterial { voxels_texture: handle },
        })));
    }
    commands.insert_resource(voxel_type_layers);

    commands.spawn((
        DirectionalLight { color: Color::srgb(0.98, 0.95, 0.82), shadows_enabled: true, ..Default::default() },
        Transform::default().looking_at(Vec3::new(-0.15, -0.1, 0.15), Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.98, 0.95, 0.82),
        brightness: 100.0,
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::resolve_blocks_dir;
    use std::path::Path;

    #[test]
    fn test_resolve_blocks_dir_with_cargo_manifest() {
        let blocks_path = resolve_blocks_dir();
        // When tests run, CARGO_MANIFEST_DIR should be set to crates/client.
        // The resolved path should end with assets/blocks.
        assert!(
            blocks_path.ends_with("assets/blocks"),
            "blocks_path should end with assets/blocks, got {:?}",
            blocks_path
        );

        // Verify the actual directory and asset exist (crates/client/../../assets/blocks -> assets/blocks).
        assert!(
            blocks_path.exists(),
            "resolved blocks directory should exist at {:?}",
            blocks_path
        );

        // Check that the expected dirt.png texture exists.
        let dirt_texture = blocks_path.join("dirt.png");
        assert!(dirt_texture.exists(), "dirt.png should exist at {:?}", dirt_texture);
    }
}
