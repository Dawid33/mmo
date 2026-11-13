//! Self-screenshot probe for the pulled (mesh-free) render path. Instead of
//! booting the whole game (whose first-person camera aim is not deterministic
//! frame-to-frame, so an automated probe can't rely on terrain being in view),
//! this builds a *controlled* scene that drives the exact production render path
//! end to end: `VoxelPullPlugin` + a `BlockArrayTexture` + a `ChunkQuads` entity
//! whose records come from the real `records::pack_record` encoder, viewed by a
//! fixed camera. It then screenshots and asserts a non-trivial fraction of
//! pixels read as textured geometry rather than the clear color, and that
//! the alpha-tested cutout quad (the leaves' pipelines) drew too. A tumbling
//! voxel body is stood in for by a yellow box meshed by the real mesher, as a
//! brick child of a ROTATED parent — the shape `renderer::voxel_bodies`
//! spawns — so the screenshot shows rotated quads with per-face shading
//! (transformed normals) and the rotation-aware cull bounds keeping it drawn.
//! It is upside down, its bottom faces up: rotated chunks are never
//! face-culled (the cull shader's per-direction test is in world axes).
//!
//! Modeled on `examples/gizmo_smoke.rs` (self-screenshot + exit). The records
//! encoder is the same one the meshing/LOD pipelines feed into `ChunkQuads`, so
//! a green frame here means the whole extract -> prepare -> queue -> draw chain
//! works against the locked `assets/shaders/voxel_pull.wgsl` layout.

use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use client::renderer::config::{FarRenderBackend, NearRenderBackend, TerrainRenderConfig};
use client::renderer::meshing::binary::{mesh_quads, CutoutTable, Face, PaddedVoxels, Quad, PADDED};
use client::renderer::meshing::records::{pack_record, QuadRecord};
use client::renderer::voxel_pull::gpu::{
    drain_near_telemetry_results, NearRendererMetrics, NearTelemetryEnabled,
};
use client::renderer::voxel_pull::{BlockArrayTexture, ChunkQuads, VoxelPullPlugin};
use game::{BlockId, Voxel};

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
const LAYER_COLORS: [[u8; 4]; 3] = [
    [90, 140, 60, 255],
    [135, 100, 70, 255],
    [140, 132, 120, 255],
];
/// The cutout layer (index 3, after `LAYER_COLORS`): a magenta checkerboard
/// whose odd texels are alpha 0, like a leaf texture's holes. Its quad is
/// drawn by the cutout pipelines (forward, depth prepass, shadow).
const CUTOUT_COLOR: [u8; 4] = [210, 30, 170, 255];
/// Minimum fraction of cutout-magenta pixels that counts as "the cutout
/// pass drew".
const MIN_CUTOUT_FRACTION: f32 = 0.005;
const TEX_SIZE: u32 = 16;
/// The voxel body's layer (index 4, after the cutout layer): a yellow no
/// terrain layer shares, so its pixels are counted on their own.
const BODY_COLOR: [u8; 4] = [225, 200, 30, 255];
const BODY_LAYER: u32 = LAYER_COLORS.len() as u32 + 1;
/// Minimum fraction of body-yellow pixels that counts as "the rotated body
/// drew". The body is turned upside down, so its local −Y faces point up at
/// the camera: with them face-culled by world axis (the bug fixed by
/// `prepare::chunk_face_ends`) it measured 0.050, drawn 0.124 (Intel Iris Xe).
const MIN_BODY_FRACTION: f32 = 0.08;

#[derive(Resource, Default)]
struct ProbeResult(Option<bool>);

#[derive(Resource, Clone)]
struct ProbeNearIndirect(Arc<AtomicU32>);

fn capture_near_indirect(metrics: Res<NearRendererMetrics>, indirect: Res<ProbeNearIndirect>) {
    if let Some(count) = metrics.indirect_draw_count {
        indirect
            .0
            .fetch_max(count.saturating_add(1), Ordering::AcqRel);
    }
}

fn main() {
    // Pin the asset root to this crate's manifest dir at compile time so bevy
    // finds the repo-root asset tree (voxel_pull.wgsl) whether launched via
    // `cargo run` or as a bare binary. Base + "../../assets" -> repo/assets.
    // SAFETY: single-threaded, before any bevy plugin reads the env.
    unsafe {
        std::env::set_var("BEVY_ASSET_ROOT", env!("CARGO_MANIFEST_DIR"));
    }

    let indirect = ProbeNearIndirect(Arc::new(AtomicU32::new(0)));
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
            })
            .set(client::renderer::render_plugin()),
    )
    .insert_resource(ClearColor(CLEAR))
    .insert_resource(TerrainRenderConfig {
        near: NearRenderBackend::GpuDriven,
        far: FarRenderBackend::Raster,
    })
    .insert_resource(indirect.clone())
    .add_plugins(VoxelPullPlugin)
    // The arena bind group also binds the designation overlay texture
    // (group(1) binding 7); without it nothing is drawn.
    .add_plugins(client::renderer::fortress_view::FortressOverlayPlugin)
    .init_resource::<ProbeResult>()
    .add_systems(Startup, setup)
    .add_systems(Update, probe);
    app.get_sub_app_mut(bevy::render::RenderApp)
        .expect("DefaultPlugins creates the render app")
        .init_resource::<NearTelemetryEnabled>()
        .insert_resource(indirect)
        .add_systems(
            bevy::render::Render,
            (drain_near_telemetry_results, capture_near_indirect)
                .chain()
                .in_set(bevy::render::RenderSystems::Cleanup),
        );
    let app_exit = app.run();
    if let AppExit::Error(code) = app_exit {
        std::process::exit(i32::from(code.get()));
    }
}

/// A small 2d-array texture: the solid warm `LAYER_COLORS`, then the cutout
/// checkerboard. Mirrors the D2Array-view fixup `renderer/mod.rs` applies so
/// the binding stays an array even with few layers.
fn array_texture(images: &mut Assets<Image>) -> Handle<Image> {
    let layers = BODY_LAYER + 1;
    let mut data = Vec::with_capacity((TEX_SIZE * TEX_SIZE * layers * 4) as usize);
    for color in LAYER_COLORS {
        for _ in 0..(TEX_SIZE * TEX_SIZE) {
            data.extend_from_slice(&color);
        }
    }
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            let [r, g, b, _] = CUTOUT_COLOR;
            data.extend_from_slice(&[r, g, b, if (x + y) % 2 == 0 { 255 } else { 0 }]);
        }
    }
    // The body: yellow with a darker one-texel border, so the texture's
    // block grid shows which way it is laid on each rotated face.
    for y in 0..TEX_SIZE {
        for x in 0..TEX_SIZE {
            let edge = x == 0 || y == 0 || x == TEX_SIZE - 1 || y == TEX_SIZE - 1;
            let [r, g, b, a] = BODY_COLOR;
            data.extend_from_slice(&if edge { [r / 2, g / 2, b / 2, a] } else { [r, g, b, a] });
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: TEX_SIZE,
            height: TEX_SIZE,
            depth_or_array_layers: layers,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..default()
    });
    // Repeat, like the real block texture (`renderer/mod.rs`): UVs run in
    // blocks, past 0..1 (the cutout checkerboard would clamp to one row).
    let mut sampler = bevy::image::ImageSamplerDescriptor::linear();
    sampler.address_mode_u = bevy::image::ImageAddressMode::Repeat;
    sampler.address_mode_v = bevy::image::ImageAddressMode::Repeat;
    image.sampler = bevy::image::ImageSampler::Descriptor(sampler);
    images.add(image)
}

/// Records for a little scene: a full-chunk floor, a raised platform (with an AO
/// gradient so the shading term is visible), and a back wall.
fn scene_records() -> Vec<QuadRecord> {
    let quads = [
        // Floor across the whole 0..32 footprint, uniform bright AO. Layer 0.
        (
            Quad {
                min: [0, 0, 0],
                w: 31,
                h: 31,
                face: Face::PosY,
                block: BlockId(1),
                ao: [3, 3, 3, 3],
                cutout: false,
                interior: false,
            },
            0,
        ),
        // Raised platform with a corner-to-corner AO gradient. Layer 1.
        (
            Quad {
                min: [8, 8, 8],
                w: 15,
                h: 15,
                face: Face::PosY,
                block: BlockId(2),
                ao: [3, 2, 1, 0],
                cutout: false,
                interior: false,
            },
            1,
        ),
        // Back wall facing +Z. Layer 2.
        (
            Quad {
                min: [0, 0, 0],
                w: 31,
                h: 15,
                face: Face::PosZ,
                block: BlockId(3),
                ao: [2, 3, 1, 2],
                cutout: false,
                interior: false,
            },
            2,
        ),
        // A see-through (cutout) wall in front of the back wall, facing the
        // camera: records keep the opaque-then-cutout order. Layer 3.
        (
            Quad {
                min: [4, 0, 20],
                w: 24,
                h: 12,
                face: Face::PosZ,
                block: BlockId(4),
                ao: [3, 3, 3, 3],
                cutout: true,
                interior: false,
            },
            3,
        ),
    ];
    quads
        .iter()
        .map(|(q, layer)| pack_record(q, *layer))
        .collect()
}

/// The body's one brick: a 2 × 1 × 1 block box (32 × 16 × 16 voxels) meshed
/// by the production mesher from an air-padded brick.
fn body_records() -> Vec<QuadRecord> {
    let mut padded = PaddedVoxels(vec![Voxel::default(); PADDED * PADDED * PADDED]);
    for z in 0..16 {
        for y in 0..16 {
            for x in 0..32 {
                padded.0[PaddedVoxels::index(x + 1, y + 1, z + 1)] = Voxel::new(BlockId(5));
            }
        }
    }
    mesh_quads(&padded, &CutoutTable::NONE).iter().map(|q| pack_record(q, BODY_LAYER)).collect()
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

    // The voxel body: a parent posed at the box's centre (its "centre of
    // mass"), turned about all three axes — upside down (pitch 2.6 rad), so
    // its −Y faces point up — with the brick child at minus the centre —
    // `voxel_bodies::brick_translation`.
    let body = commands
        .spawn((
            Transform::from_xyz(42.0, 10.0, -6.0).with_rotation(Quat::from_euler(EulerRot::YXZ, 0.6, 2.6, 0.35)),
            Visibility::Visible,
        ))
        .id();
    commands.spawn((
        ChunkQuads(Arc::new(body_records())),
        Transform::from_translation(-Vec3::new(16.0, 8.0, 8.0)),
        bevy::camera::primitives::Aabb {
            center: bevy::math::Vec3A::splat(16.0),
            half_extents: bevy::math::Vec3A::splat(16.0),
        },
        bevy::camera::visibility::NoAutoAabb,
        ChildOf(body),
    ));

    // Fixed camera looking down the scene from above/behind +Z. DepthPrepass +
    // OcclusionCulling mirror the real camera (visibility.rs::enable_native_occlusion)
    // so the probe exercises the pulled-quad depth prepass (Task 6) — and, because
    // the prepass adds the DEPTH_PREPASS bit to the forward view layout, it also
    // validates that the forward pipeline derives its group(0) layout from the
    // view's prepass state (a mismatch fails loudly at pipeline creation).
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(16.0, 30.0, 58.0).looking_at(Vec3::new(16.0, 6.0, 14.0), Vec3::Y),
        bevy::core_pipeline::prepass::DepthPrepass,
        bevy::render::occlusion_culling::OcclusionCulling,
    ));

    // Sun, shadows on — exercises the lit fragment's directional-light +
    // shadow-receive path (Task 5), not just the ambient term.
    commands.spawn((
        DirectionalLight {
            shadow_maps_enabled: true,
            ..default()
        },
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
fn analyze_screenshot(
    trigger: On<ScreenshotCaptured>,
    mut result: ResMut<ProbeResult>,
    indirect: Res<ProbeNearIndirect>,
) {
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
    let mut cutout = 0u32;
    let mut body = 0u32;
    for p in rgb.pixels() {
        let [r, g, b] = p.0;
        // Warm/neutral geometry: blue is not clearly the largest channel.
        if (b as i32) <= (r as i32).max(g as i32) {
            terrain += 1;
        }
        // The cutout wall's magenta: red and blue both well above green.
        if (r as i32) > 2 * (g as i32) + 20 && (b as i32) > 2 * (g as i32) + 20 {
            cutout += 1;
        }
        // The body's yellow: red and green both well above blue, and close.
        let (r, g, b) = (r as i32, g as i32, b as i32);
        if r > 2 * b + 20 && g > 2 * b + 20 && (r - g).abs() * 4 < r {
            body += 1;
        }
    }
    let fraction = terrain as f32 / total;
    let cutout_fraction = cutout as f32 / total;
    let body_fraction = body as f32 / total;
    info!(
        "pull_smoke: {:.0} px, terrain fraction {:.3} (threshold {:.2}), cutout fraction {:.4} (threshold {:.3})",
        total, fraction, MIN_TERRAIN_FRACTION, cutout_fraction, MIN_CUTOUT_FRACTION
    );
    let encoded_indirect_count = indirect.0.load(Ordering::Acquire);
    let indirect_count = encoded_indirect_count.saturating_sub(1);
    info!("pull_smoke: maximum observed GPU indirect count {indirect_count}");
    info!("pull_smoke: rotated body fraction {body_fraction:.4} (threshold {MIN_BODY_FRACTION:.3})");
    result.0 = Some(
        fraction >= MIN_TERRAIN_FRACTION
            && cutout_fraction >= MIN_CUTOUT_FRACTION
            && body_fraction >= MIN_BODY_FRACTION
            && encoded_indirect_count > 1,
    );
}
