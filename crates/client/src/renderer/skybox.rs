//! Procedural gradient skybox. A small cubemap is generated at startup
//! (no asset file, so it works identically on wasm) and attached to every
//! `Camera3d` by a catch-up system — cameras spawn dynamically (avatar.rs,
//! bridge.rs) and can lose/regain `Camera3d`, so per-spawn-site insertion
//! would leak coverage.

use bevy::asset::RenderAssetUsages;
use bevy::core_pipeline::Skybox;
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};

/// Edge length of each generated cubemap face, in texels. The gradient is
/// low-frequency, so a small face renders smooth.
pub const FACE_SIZE: u32 = 64;
/// Skybox luminance scale in cd/m² (Bevy tonemaps physical units; ~2000 reads
/// as a bright day sky against the default exposure).
pub const BRIGHTNESS: f32 = 2000.0;
/// Sky color straight up.
pub const ZENITH: [u8; 3] = [70, 120, 215];
/// Sky color at the horizon.
pub const HORIZON: [u8; 3] = [165, 200, 235];
/// Color below the horizon (fog-like, seen from cliffs/underwater edges).
pub const GROUND: [u8; 3] = [95, 110, 135];

/// The generated cubemap, shared by every camera's `Skybox` component.
#[derive(Resource)]
pub struct SkyboxCubemap(pub Handle<Image>);

fn lerp3(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    core::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

/// Gradient color for a view direction with normalized world-space Y
/// component `y` (-1 = straight down, +1 = straight up): horizon → zenith
/// above the horizon (smoothstepped), a quick fade to `GROUND` below it.
fn sky_color(y: f32) -> [u8; 3] {
    if y >= 0.0 {
        let t = y * y * (3.0 - 2.0 * y); // smoothstep
        lerp3(HORIZON, ZENITH, t)
    } else {
        lerp3(HORIZON, GROUND, -y * 3.0)
    }
}

/// RGBA texel data for the 6 cubemap faces in wgpu layer order
/// (+X, -X, +Y, -Y, +Z, -Z). Only the direction's Y component drives the
/// color (the gradient is rotationally symmetric about the vertical axis),
/// but each face's full direction is computed for clarity.
pub fn build_cubemap_data(size: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity((size * size * 6 * 4) as usize);
    for face in 0..6u32 {
        for row in 0..size {
            // s/t in [-1, 1] at texel centers; t grows downward in the image.
            let t = (2 * row + 1) as f32 / size as f32 - 1.0;
            for col in 0..size {
                let s = (2 * col + 1) as f32 / size as f32 - 1.0;
                let dir: [f32; 3] = match face {
                    0 => [1.0, -t, -s],  // +X
                    1 => [-1.0, -t, s],  // -X
                    2 => [s, 1.0, t],    // +Y
                    3 => [s, -1.0, -t],  // -Y
                    4 => [s, -t, 1.0],   // +Z
                    _ => [-s, -t, -1.0], // -Z
                };
                let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
                let [r, g, b] = sky_color(dir[1] / len);
                data.extend_from_slice(&[r, g, b, 255]);
            }
        }
    }
    data
}

/// Build the cubemap image and stash its handle; runs once at startup.
pub fn setup_skybox(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let mut image = Image::new(
        Extent3d { width: FACE_SIZE, height: FACE_SIZE, depth_or_array_layers: 6 },
        TextureDimension::D2,
        build_cubemap_data(FACE_SIZE),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    // 6-layer D2 defaults to a D2Array view; the skybox shader binds a Cube.
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..Default::default()
    });
    commands.insert_resource(SkyboxCubemap(images.add(image)));
}

/// Attach the skybox to any camera that doesn't have one yet. `Option`al
/// resource so headless test apps without `setup_skybox` stay valid.
pub fn attach_skybox(
    mut commands: Commands,
    cubemap: Option<Res<SkyboxCubemap>>,
    cameras: Query<Entity, (With<Camera3d>, Without<Skybox>)>,
) {
    let Some(cubemap) = cubemap else { return };
    for e in &cameras {
        commands.entity(e).insert(Skybox {
            image: Some(cubemap.0.clone()),
            brightness: BRIGHTNESS,
            ..Default::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Texel helper: RGBA of (face, row, col) in the generated data.
    fn texel(data: &[u8], size: u32, face: u32, row: u32, col: u32) -> [u8; 3] {
        let i = (((face * size + row) * size + col) * 4) as usize;
        [data[i], data[i + 1], data[i + 2]]
    }

    #[test]
    fn cubemap_data_has_six_rgba_faces() {
        let data = build_cubemap_data(FACE_SIZE);
        assert_eq!(data.len(), (FACE_SIZE * FACE_SIZE * 6 * 4) as usize);
        assert!(data.iter().skip(3).step_by(4).all(|&a| a == 255), "opaque alpha");
    }

    #[test]
    fn gradient_endpoints_hit_the_named_colors() {
        let size = 16;
        let data = build_cubemap_data(size);
        let mid = size / 2;
        // +Y face center looks straight up; -Y straight down. Centers sit half
        // a texel off-axis, so allow ±2 per channel.
        let close = |got: [u8; 3], want: [u8; 3]| {
            got.iter().zip(want).all(|(&g, w)| (g as i16 - w as i16).abs() <= 2)
        };
        assert!(close(texel(&data, size, 2, mid, mid), ZENITH), "up = zenith");
        assert!(close(texel(&data, size, 3, mid, mid), GROUND), "down = ground");
        // On a side face the horizon (t == 0) falls between rows mid-1 and
        // mid; the row just above looks up by half a texel, where the
        // smoothstepped sky branch is still within a couple of counts of the
        // horizon color (the below-horizon fade is 3x steeper, so row mid
        // would already be well into the ground blend).
        assert!(close(texel(&data, size, 0, mid - 1, mid), HORIZON), "level = horizon");
    }

    #[test]
    fn cameras_get_a_skybox_attached_once_set_up() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Image>()
            .add_systems(Startup, setup_skybox)
            .add_systems(Update, attach_skybox);
        let cam = app.world_mut().spawn(Camera3d::default()).id();
        app.update();
        assert!(app.world().entity(cam).contains::<Skybox>());
        // A camera spawned later is picked up on the next frame.
        let late = app.world_mut().spawn(Camera3d::default()).id();
        app.update();
        assert!(app.world().entity(late).contains::<Skybox>());
    }

    #[test]
    fn attach_without_cubemap_resource_is_a_no_op() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_systems(Update, attach_skybox);
        let cam = app.world_mut().spawn(Camera3d::default()).id();
        app.update();
        assert!(!app.world().entity(cam).contains::<Skybox>());
    }
}
