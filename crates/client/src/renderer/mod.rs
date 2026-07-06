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

mod avatar;
mod bridge;
mod hud;
pub mod convert;
mod input;
mod interpolate;
pub mod meshing;
mod voxel_material;
use voxel_material::StandardVoxelMaterial;

#[derive(Resource)]
pub struct ClientUpdates(pub Receiver<ClientUpdateEvent>);

#[derive(Resource)]
pub struct GameEvents(pub Sender<GameEventKind>);

#[derive(Resource, Default)]
pub struct LocalPlayer(pub Option<ClientId>);

/// Maps a simulation `VoxelType` to the array-texture layer index that renders it.
#[derive(Resource, Default, Clone)]
pub struct VoxelTypeLayers(pub BTreeMap<game::VoxelType, u32>);

pub struct SimBridgePlugin {
    pub client_recv: Receiver<ClientUpdateEvent>,
    pub game_send: Sender<GameEventKind>,
}

impl Plugin for SimBridgePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
                MaterialPlugin::<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>::default(),
                bevy::diagnostic::FrameTimeDiagnosticsPlugin::default(),
            ))
            .insert_resource(ClientUpdates(self.client_recv.clone()))
            .insert_resource(GameEvents(self.game_send.clone()))
            .init_resource::<LocalPlayer>()
            .init_resource::<bridge::Regions>()
            .init_resource::<bridge::RegionRoots>()
            .init_resource::<bridge::SimEntityMap>()
            .init_resource::<hud::HudStatus>()
            .init_resource::<avatar::AvatarAssets>()
            .add_systems(
                PreUpdate,
                (
                    input::forward_input.after(bevy::input::InputSystems),
                    (bridge::drain_client_updates, bridge::drain_region_updates, bridge::dedupe_ghosts).chain(),
                )
                    .chain(),
            )
            .add_systems(Startup, (setup_scene, hud::setup_hud))
            .add_systems(
                Update,
                (
                    meshing::queue_meshing,
                    meshing::apply_meshed_chunks,
                    interpolate::interpolate_transforms,
                    avatar::attach_avatars,
                    hud::toggle_debug,
                    hud::update_debug_text,
                    hud::update_crosshair_visibility,
                ),
            );
    }
}

/// Resolve the blocks asset directory; must mirror AssetPlugin.file_path resolution in main.rs.
///
/// Base-path priority mirrors bevy_asset's `get_base_path` exactly (see
/// `bevy_asset::io::file::get_base_path`): `BEVY_ASSET_ROOT` env var, then
/// `CARGO_MANIFEST_DIR` (set by cargo at runtime), then the running executable's
/// parent directory. Each base is then joined with the same relative path
/// `AssetPlugin { file_path: "../../assets", .. }` uses in main.rs, plus our own
/// `blocks` subdir, so all three bases resolve consistently.
#[cfg(not(target_arch = "wasm32"))]
fn resolve_blocks_dir() -> std::path::PathBuf {
    use std::path::PathBuf;

    let base = if let Ok(root) = std::env::var("BEVY_ASSET_ROOT") {
        PathBuf::from(root)
    } else if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        PathBuf::from(manifest_dir)
    } else if let Ok(exe_path) = std::env::current_exe() {
        match exe_path.parent() {
            Some(exe_dir) => exe_dir.to_path_buf(),
            // No parent (e.g. exe at filesystem root): fall back to cwd-relative.
            None => return PathBuf::from("assets/blocks"),
        }
    } else {
        // Final fallback: cwd-relative (original behavior).
        return PathBuf::from("assets/blocks");
    };

    base.join("../../assets/blocks")
}

/// Block textures for targets with no filesystem (wasm): embedded at compile
/// time. The `embedded_block_textures_match_assets_dir` test keeps this list
/// in sync with assets/blocks/*.png.
#[cfg(any(target_arch = "wasm32", test))]
const EMBEDDED_BLOCK_TEXTURES: &[(&str, &[u8])] = &[(
    "dirt.png",
    include_bytes!("../../../../assets/blocks/dirt.png"),
)];

fn setup_scene(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut voxel_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>>,
) {
    let mut layers: Vec<image::RgbaImage> = Vec::new();
    let mut layer_names: Vec<String> = Vec::new();
    let mut sorted: BTreeMap<String, image::RgbaImage> = BTreeMap::new();
    #[cfg(not(target_arch = "wasm32"))]
    {
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
    }
    // No filesystem in the browser: HTTP can't enumerate a directory, so wasm
    // builds carry the block textures embedded in the binary instead.
    #[cfg(target_arch = "wasm32")]
    for (name, bytes) in EMBEDDED_BLOCK_TEXTURES {
        match image::load_from_memory(bytes) {
            Ok(decoded) => {
                sorted.insert((*name).to_string(), decoded.to_rgba8());
            }
            Err(e) => warn!("failed to decode embedded block texture {name}: {e:?}"),
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
        voxel_type_layers.0.insert(game::VoxelType::Black, black_layer);

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
    use super::{resolve_blocks_dir, EMBEDDED_BLOCK_TEXTURES};

    #[test]
    fn bevy_ui_features_enabled() {
        // Compiles only if bevy_ui / bevy_text features are on. Constructing the
        // types (not rendering) is enough to prove the feature gate.
        use bevy::prelude::*;
        let _node = Node::default();
        let _text = Text::new("hud");
        let _white = BackgroundColor(Color::WHITE);
        let _mark = bevy::ui::IsDefaultUiCamera;
    }

    #[test]
    fn embedded_block_textures_match_assets_dir() {
        let dir = resolve_blocks_dir();
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("assets/blocks must exist for this test")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("png"))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        on_disk.sort();
        let mut embedded: Vec<String> =
            EMBEDDED_BLOCK_TEXTURES.iter().map(|(n, _)| (*n).to_string()).collect();
        embedded.sort();
        assert_eq!(
            embedded, on_disk,
            "EMBEDDED_BLOCK_TEXTURES is out of sync with assets/blocks/*.png — update the const in renderer/mod.rs"
        );
    }

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
