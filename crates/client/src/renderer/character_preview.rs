//! The player's model in the inventory's character frame
//! (`ui/inventory.slint`): a second camera whose viewport is the frame's
//! inside, drawn after the world camera — so over the Slint layers, which
//! are composited into that camera's picture — and seeing only its own
//! render layer: the rig's model, standing in its idle clip, and one light.
//!
//! The camera carries [`FrontendCamera`], whose meaning here is only "this
//! view draws no world": the custom terrain passes bin into every view and
//! skip the ones with that marker. It clears nothing (a clear would wipe the
//! whole window, not the viewport); the frame's dark fill under it is Slint's.
//!
//! Made on first use and kept for the app's life (nothing of a world is in
//! it); inactive whenever the frame is not on screen.

use bevy::camera::Viewport;
use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use game::anim::{Animator, RigId};

use super::model::{BbModel, BbModelPart, BbModelSpawned};
use super::slint_overlay::SlintLayer;
use crate::session::FrontendCamera;

/// The render layer only the preview camera sees.
pub const CHARACTER_PREVIEW_LAYER: usize = 30;
/// After every world camera (whose picture holds the UI), before the
/// frontend's (`session::FRONTEND_CAMERA_ORDER`), which never runs beside it.
pub const CHARACTER_PREVIEW_CAMERA_ORDER: isize = 10;
/// Vertical field of view, degrees: narrow, so the model is not distorted.
pub const CHARACTER_PREVIEW_FOV_DEG: f32 = 30.0;
/// Room left around the model, as a share of its size.
pub const CHARACTER_PREVIEW_MARGIN: f32 = 1.15;
/// The model is seen from in front, this far round to its side, degrees.
pub const CHARACTER_PREVIEW_TURN_DEG: f32 = 25.0;
/// The key light, lux, and the fill around it.
pub const CHARACTER_PREVIEW_LIGHT_LUX: f32 = 9_000.0;
pub const CHARACTER_PREVIEW_AMBIENT: f32 = 500.0;

/// The preview's camera.
#[derive(Component)]
pub struct CharacterPreviewCamera;

/// The preview's model root, and its idle animation's state.
#[derive(Component)]
pub struct CharacterPreviewModel(Animator);

pub struct CharacterPreviewPlugin;

impl Plugin for CharacterPreviewPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (sync_character_preview, pose_character_preview).chain(),
        );
    }
}

fn spawn_preview(commands: &mut Commands, assets: &AssetServer) {
    let layer = RenderLayers::layer(CHARACTER_PREVIEW_LAYER);
    let rig = game::anim::rig(RigId::Dwarf);
    commands.spawn((
        CharacterPreviewModel(Animator::idle(rig.id)),
        BbModel(assets.load(rig.asset_path)),
        Transform::default(),
        Visibility::default(),
        layer.clone(),
        Name::new("character preview model"),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: CHARACTER_PREVIEW_LIGHT_LUX,
            shadow_maps_enabled: false,
            ..default()
        },
        // From over the viewer's shoulder, down onto the model's front.
        Transform::from_xyz(-1.0, 1.5, -2.0).looking_at(Vec3::ZERO, Vec3::Y),
        layer.clone(),
        Name::new("character preview light"),
    ));
    commands.spawn((
        CharacterPreviewCamera,
        FrontendCamera,
        Camera3d::default(),
        Camera {
            is_active: false,
            order: CHARACTER_PREVIEW_CAMERA_ORDER,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        Projection::Perspective(PerspectiveProjection {
            fov: CHARACTER_PREVIEW_FOV_DEG.to_radians(),
            ..default()
        }),
        AmbientLight {
            brightness: CHARACTER_PREVIEW_AMBIENT,
            ..default()
        },
        layer,
        // Present-but-empty, so the world's `attach_skybox` / `attach_fog`
        // leave this camera alone (as the frontend camera's).
        bevy::core_pipeline::Skybox::default(),
        bevy::pbr::DistanceFog {
            color: Color::NONE,
            ..default()
        },
        Name::new("character preview camera"),
    ));
}

/// Where the camera stands to fit the model (`lo..hi`, model space) in a
/// viewport of `aspect` (width / height): in front of it (the model faces
/// -Z), turned [`CHARACTER_PREVIEW_TURN_DEG`] to its side, looking at its
/// centre.
pub fn preview_camera_transform(lo: Vec3, hi: Vec3, aspect: f32) -> Transform {
    let centre = (lo + hi) / 2.0;
    let size = hi - lo;
    let tan = (CHARACTER_PREVIEW_FOV_DEG.to_radians() / 2.0).tan();
    let half_height = size.y / 2.0 * CHARACTER_PREVIEW_MARGIN;
    // Turned, the model's footprint is at most its diagonal wide.
    let half_width = size.x.hypot(size.z) / 2.0 * CHARACTER_PREVIEW_MARGIN;
    let distance = (half_height / tan).max(half_width / (tan * aspect.max(0.01))) + size.z / 2.0;
    let offset = Quat::from_rotation_y(CHARACTER_PREVIEW_TURN_DEG.to_radians())
        * Vec3::new(0.0, 0.0, -distance);
    Transform::from_translation(centre + offset).looking_at(centre, Vec3::Y)
}

/// The frame's rectangle from the game UI into the camera's viewport; the
/// camera is active only while the frame is on screen.
fn sync_character_preview(
    mut commands: Commands,
    assets: Res<AssetServer>,
    layer: Option<NonSend<SlintLayer<slint_ui::GameUi>>>,
    window: Query<&Window, With<PrimaryWindow>>,
    state: Option<Res<State<crate::session::AppState>>>,
    in_play: Option<Res<crate::session::InPlay>>,
    mut cameras: Query<(&mut Camera, &mut Transform), With<CharacterPreviewCamera>>,
) {
    let rect = (|| {
        let layer = layer.as_ref()?;
        let window = window.single().ok()?;
        if crate::session::frontend_has_screen(state.as_deref(), in_play.as_deref()) {
            return None;
        }
        let ui = layer.overlay.ui();
        if !ui.get_inventory_character_shown() {
            return None;
        }
        // UI pixels → physical pixels, cut to the window.
        let scale = layer.ui_scale() * window.scale_factor();
        let (max_x, max_y) = (
            window.physical_width() as f32,
            window.physical_height() as f32,
        );
        let x0 = (ui.get_inventory_character_x() * scale)
            .round()
            .clamp(0.0, max_x);
        let y0 = (ui.get_inventory_character_y() * scale)
            .round()
            .clamp(0.0, max_y);
        let x1 = ((ui.get_inventory_character_x() + ui.get_inventory_character_width()) * scale)
            .round()
            .clamp(0.0, max_x);
        let y1 = ((ui.get_inventory_character_y() + ui.get_inventory_character_height()) * scale)
            .round()
            .clamp(0.0, max_y);
        (x1 - x0 >= 1.0 && y1 - y0 >= 1.0).then(|| {
            (
                UVec2::new(x0 as u32, y0 as u32),
                UVec2::new((x1 - x0) as u32, (y1 - y0) as u32),
            )
        })
    })();
    let Ok((mut camera, mut transform)) = cameras.single_mut() else {
        if rect.is_some() {
            spawn_preview(&mut commands, &assets);
        }
        return;
    };
    let Some((position, size)) = rect else {
        if camera.is_active {
            camera.is_active = false;
        }
        return;
    };
    let unchanged = camera
        .viewport
        .as_ref()
        .is_some_and(|v| v.physical_position == position && v.physical_size == size);
    if !unchanged {
        camera.viewport = Some(Viewport {
            physical_position: position,
            physical_size: size,
            ..default()
        });
        let (lo, hi) = game::anim::rig(RigId::Dwarf)
            .model
            .bounds()
            .unwrap_or(([-8.0; 3], [8.0; 3]));
        *transform = preview_camera_transform(
            Vec3::from(lo),
            Vec3::from(hi),
            size.x as f32 / size.y as f32,
        );
    }
    if !camera.is_active {
        camera.is_active = true;
    }
}

/// The model's idle clip, while it is on screen (as `rigs::pose_rig_models`
/// poses a sim entity's).
fn pose_character_preview(
    time: Res<Time>,
    cameras: Query<&Camera, With<CharacterPreviewCamera>>,
    mut models: Query<(&mut CharacterPreviewModel, &BbModelSpawned)>,
    mut parts: Query<&mut Transform, With<BbModelPart>>,
) {
    if !cameras.iter().any(|c| c.is_active) {
        return;
    }
    for (mut model, spawned) in &mut models {
        model.0.advance(time.delta_secs());
        let local = game::anim::rig(model.0.rig)
            .model
            .local_pose(&game::anim::sample(&model.0, 0.0));
        if local.len() != spawned.0.len() {
            continue;
        }
        for ((t, r, s), &e) in local.into_iter().zip(&spawned.0) {
            if let Ok(mut part) = parts.get_mut(e) {
                part.set_if_neq(Transform {
                    translation: Vec3::from(t),
                    rotation: Quat::from_array(r),
                    scale: Vec3::from(s),
                });
            }
        }
    }
}
