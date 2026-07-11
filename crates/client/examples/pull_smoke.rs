//! Self-screenshot probe for the pulled (mesh-free) render path. Instead of
//! booting the whole game (whose first-person camera aim is not deterministic
//! frame-to-frame, so an automated probe can't rely on terrain being in view),
//! this builds a *controlled* scene that drives the exact production render path
//! end to end: `VoxelPullPlugin` + a `BlockArrayTexture` + a `ChunkQuads` entity
//! whose records come from the real `records::pack_record` encoder, viewed by a
//! fixed camera. It then screenshots and asserts a non-trivial fraction of
//! pixels read as textured geometry rather than the clear color.
//!
//! Modeled on `examples/gizmo_smoke.rs` (self-screenshot + exit). The records
//! encoder is the same one the meshing/LOD pipelines feed into `ChunkQuads`, so
//! a green frame here means the whole extract -> prepare -> queue -> draw chain
//! works against the locked `assets/shaders/voxel_pull.wgsl` layout.

use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use client::renderer::meshing::binary::{Face, Quad};
use client::renderer::meshing::records::{pack_record, QuadRecord};
use client::renderer::voxel_pull::{BlockArrayTexture, ChunkQuads, VoxelPullPlugin};
use game::BlockId;

const SCREENSHOT_PATH: &str = "/tmp/pull_smoke.png";
/// Frames to run before screenshotting — enough for the pipeline to compile
/// (async) and the render world to upload the chunk buffers.
const SHOOT_FRAME: u32 = 90;
/// A clearly blue clear color, so "blue not dominant" cleanly separates the
/// warm-textured pulled quads from the background.
const CLEAR: Color = Color::srgb(0.35, 0.55, 0.9);
/// Minimum fraction of terrain-colored pixels that counts as "quads drew".
const MIN_TERRAIN_FRACTION: f32 = 0.05;

/// Warm per-layer colors for the array texture (all have blue NOT dominant, so
/// they read as terrain against the blue clear): grass-green, dirt-brown, stone.
const LAYER_COLORS: [[u8; 4]; 3] = [[90, 140, 60, 255], [135, 100, 70, 255], [140, 132, 120, 255]];
const TEX_SIZE: u32 = 16;

#[derive(Resource, Default)]
struct ProbeResult(Option<bool>);

fn main() {
    // Pin the asset root to this crate's manifest dir at compile time so bevy
    // finds the repo-root asset tree (voxel_pull.wgsl) whether launched via
    // `cargo run` or as a bare binary. Base + "../../assets" -> repo/assets.
    // SAFETY: single-threaded, before any bevy plugin reads the env.
    unsafe {
        std::env::set_var("BEVY_ASSET_ROOT", env!("CARGO_MANIFEST_DIR"));
    }

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(bevy::asset::AssetPlugin {
                file_path: "../../assets".into(),
                ..default()
            })
            .set(bevy::log::LogPlugin {
                filter: "wgpu=error,naga=warn".into(),
                ..default()
            }),
    )
    .insert_resource(ClearColor(CLEAR))
    .add_plugins(VoxelPullPlugin)
    .init_resource::<ProbeResult>()
    .add_systems(Startup, setup)
    .add_systems(Update, probe);
    app.run();
}

/// A small 3-layer 2d-array texture of solid warm colors. Mirrors the
/// D2Array-view fixup `renderer/mod.rs` applies so the binding stays an array
/// even with few layers.
fn array_texture(images: &mut Assets<Image>) -> Handle<Image> {
    let layers = LAYER_COLORS.len() as u32;
    let mut data = Vec::with_capacity((TEX_SIZE * TEX_SIZE * layers * 4) as usize);
    for color in LAYER_COLORS {
        for _ in 0..(TEX_SIZE * TEX_SIZE) {
            data.extend_from_slice(&color);
        }
    }
    let mut image = Image::new(
        Extent3d { width: TEX_SIZE, height: TEX_SIZE, depth_or_array_layers: layers },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..default()
    });
    images.add(image)
}

/// Records for a little scene: a full-chunk floor, a raised platform (with an AO
/// gradient so the shading term is visible), and a back wall.
fn scene_records() -> Vec<QuadRecord> {
    let quads = [
        // Floor across the whole 0..32 footprint, uniform bright AO. Layer 0.
        (Quad { min: [0, 0, 0], w: 31, h: 31, face: Face::PosY, block: BlockId(1), ao: [3, 3, 3, 3] }, 0),
        // Raised platform with a corner-to-corner AO gradient. Layer 1.
        (Quad { min: [8, 8, 8], w: 15, h: 15, face: Face::PosY, block: BlockId(2), ao: [3, 2, 1, 0] }, 1),
        // Back wall facing +Z. Layer 2.
        (Quad { min: [0, 0, 0], w: 31, h: 15, face: Face::PosZ, block: BlockId(3), ao: [2, 3, 1, 2] }, 2),
    ];
    quads.iter().map(|(q, layer)| pack_record(q, *layer)).collect()
}

fn setup(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    commands.insert_resource(BlockArrayTexture(array_texture(&mut images)));

    commands.spawn((
        ChunkQuads(Arc::new(scene_records())),
        Transform::default(),
        Visibility::Visible,
        bevy::camera::primitives::Aabb {
            center: bevy::math::Vec3A::splat(16.0),
            half_extents: bevy::math::Vec3A::splat(16.0),
        },
    ));

    // Fixed camera looking down the scene from above/behind +Z.
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(16.0, 30.0, 58.0).looking_at(Vec3::new(16.0, 6.0, 14.0), Vec3::Y),
    ));

    // Sun, shadows on — exercises the lit fragment's directional-light +
    // shadow-receive path (Task 5), not just the ambient term.
    commands.spawn((
        DirectionalLight { shadows_enabled: true, ..default() },
        Transform::default().looking_to(Vec3::new(-0.5, -1.0, -0.4), Vec3::Y),
    ));
}

fn probe(
    mut commands: Commands,
    result: Res<ProbeResult>,
    mut frames: Local<u32>,
    mut shot: Local<bool>,
    mut exit: MessageWriter<AppExit>,
) {
    *frames += 1;
    if !*shot && *frames >= SHOOT_FRAME {
        info!("pull_smoke: capturing screenshot at frame {}", *frames);
        commands
            .spawn(Screenshot::primary_window())
            .observe(analyze_screenshot);
        *shot = true;
    }

    if let Some(detected) = result.0 {
        if detected {
            info!("pull_smoke: pulled quads drew textured geometry — PASS");
            exit.write(AppExit::Success);
        } else {
            error!("pull_smoke: pulled quads did NOT draw — FAIL");
            exit.write(AppExit::error());
        }
        return;
    }

    if *frames > SHOOT_FRAME + 300 {
        error!("pull_smoke: screenshot never resolved — FAIL");
        exit.write(AppExit::error());
    }
}

/// Screenshot observer: save the PNG and classify pixels as terrain vs the blue
/// clear. Terrain = blue is not the dominant channel (the warm layer colors);
/// the clear color has blue clearly dominant.
fn analyze_screenshot(trigger: On<ScreenshotCaptured>, mut result: ResMut<ProbeResult>) {
    let dyn_img = match trigger.image.clone().try_into_dynamic() {
        Ok(d) => d,
        Err(e) => {
            error!("pull_smoke: could not decode screenshot: {e:?}");
            result.0 = Some(false);
            return;
        }
    };
    let rgb = dyn_img.to_rgb8();
    if let Err(e) = rgb.save(SCREENSHOT_PATH) {
        warn!("pull_smoke: failed to save screenshot to {SCREENSHOT_PATH}: {e}");
    } else {
        info!("pull_smoke: screenshot saved to {SCREENSHOT_PATH}");
    }

    let total = (rgb.width() * rgb.height()) as f32;
    let mut terrain = 0u32;
    for p in rgb.pixels() {
        let [r, g, b] = p.0;
        // Warm/neutral geometry: blue is not clearly the largest channel.
        if (b as i32) <= (r as i32).max(g as i32) {
            terrain += 1;
        }
    }
    let fraction = terrain as f32 / total;
    info!(
        "pull_smoke: {:.0} px, terrain fraction {:.3} (threshold {:.2})",
        total, fraction, MIN_TERRAIN_FRACTION
    );
    result.0 = Some(fraction >= MIN_TERRAIN_FRACTION);
}
