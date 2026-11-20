//! What the player looks at in first person: the block under the crosshair
//! within the sim's edit reach ([`LookTarget`]), found on the client from the
//! tile mirror with the sim's own ray walk (`game::edit::raycast_blocks`), so
//! it is the block a click would hit. Drawn as a thin outline around the
//! block (as Minecraft does) and described in the HUD's bottom-right pane
//! (`ui/hud.slint`).
//!
//! The description is what the block manifest knows — name, hardness, drop —
//! since blocks carry no written descriptions yet.

use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use game::{BLOCK_VOXELS, BlockId, RegionId, TilePos};

use super::commander::first_person::{BodyView, Eyes, FirstPerson};
use super::commander::tiles::TileMirror;
use super::slint_overlay::SlintLayer;

/// The outline's colour and width (logical pixels).
pub const BLOCK_OUTLINE_COLOR: Color = Color::srgba(0.0, 0.0, 0.0, 0.75);
pub const BLOCK_OUTLINE_WIDTH_PX: f32 = 2.0;
/// The outline stands this far off the block's faces, as a share of a block,
/// so it does not fight them for depth.
pub const BLOCK_OUTLINE_GROW: f32 = 0.004;

/// The block under the crosshair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LookedAt {
    pub region: RegionId,
    /// Region-local block.
    pub block: TilePos,
    pub id: BlockId,
}

/// What the player looks at; `None` out of first person, with the cursor
/// free (a menu, the inventory) or with nothing in reach.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct LookTarget(pub Option<LookedAt>);

/// The outline's own gizmo group: its line width, apart from the debug views'.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct BlockOutlineGizmos;

pub struct LookTargetPlugin;

impl Plugin for LookTargetPlugin {
    fn build(&self, app: &mut App) {
        use crate::session::SessionResetExt;
        app.init_resource::<LookTarget>()
            .add_session_reset::<LookTarget>()
            .init_gizmo_group::<BlockOutlineGizmos>()
            .add_systems(Startup, configure_outline)
            .add_systems(
                Update,
                (update_look_target, draw_block_outline, push_look_pane).chain(),
            );
    }
}

fn configure_outline(mut store: ResMut<GizmoConfigStore>) {
    let (config, _) = store.config_mut::<BlockOutlineGizmos>();
    config.line.width = BLOCK_OUTLINE_WIDTH_PX;
}

fn update_look_target(
    fp: Option<Res<FirstPerson>>,
    mirror: Option<Res<TileMirror>>,
    cursor: Query<&CursorOptions, With<PrimaryWindow>>,
    eyes: Query<&Transform, With<Eyes>>,
    mut target: ResMut<LookTarget>,
) {
    let looked = (|| {
        let (fp, mirror) = (fp?, mirror?);
        // As the crosshair: first person, the cursor locked for mouse look.
        if fp.body_view() != Some(BodyView::First) {
            return None;
        }
        if cursor.iter().next().map(|c| c.grab_mode) != Some(CursorGrabMode::Locked) {
            return None;
        }
        let eye = eyes.single().ok()?;
        let region = mirror.region()?;
        let origin = eye.translation - mirror.home_offset()?;
        let dir = eye.rotation * Vec3::NEG_Z;
        let halo = mirror.with_halo();
        let v = |p: Vec3| game::rapier3d::math::Vector::new(p.x, p.y, p.z);
        let hit = game::edit::raycast_blocks(v(origin), v(dir), game::edit::REACH, |p| {
            game::block_props().blocks_ray(game::TileSource::block(&halo, p))
        })?;
        Some(LookedAt {
            region,
            block: hit.block,
            id: game::TileSource::block(&halo, hit.block),
        })
    })();
    target.set_if_neq(LookTarget(looked));
}

fn draw_block_outline(
    target: Res<LookTarget>,
    frame: Option<Res<crate::wrap_frame::WrapFrame>>,
    mut gizmos: Gizmos<BlockOutlineGizmos>,
) {
    let Some(looked) = target.0 else { return };
    let size = BLOCK_VOXELS as f32;
    let frame = frame.as_deref().copied().unwrap_or_default();
    let centre = Vec3::from_array(frame.offset(looked.region))
        + (IVec3::from_array(looked.block).as_vec3() + 0.5) * size;
    gizmos.cube(
        Transform::from_translation(centre)
            .with_scale(Vec3::splat(size * (1.0 + BLOCK_OUTLINE_GROW))),
        BLOCK_OUTLINE_COLOR,
    );
}

/// A manifest name as a player reads it: `oak_log` → `Oak log`.
pub fn display_name(name: &str) -> String {
    let spaced = name.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => spaced,
    }
}

/// The pane's title and lines for a block: what the manifest says of it.
pub fn describe_block(id: BlockId) -> (String, Vec<String>) {
    let props = game::block_props();
    let title = display_name(props.name_of(id).unwrap_or("unknown"));
    let mut lines = Vec::new();
    if props.is_trunk(id) {
        lines.push("Tree trunk".to_owned());
    }
    lines.push(format!(
        "Hardness {}",
        f32::from(props.hardness_centi(id)) / 100.0
    ));
    lines.push(match props.drop(id) {
        game::BlockDrop::Same => format!("Drops {title}"),
        game::BlockDrop::Nothing => "Drops nothing".to_owned(),
        game::BlockDrop::Item(item) => format!("Drops {}", display_name(game::item_name(item))),
    });
    (title, lines)
}

/// The looked-at block into the HUD's pane, when the block's kind changes.
fn push_look_pane(
    hud: Option<NonSend<SlintLayer<slint_ui::HudUi>>>,
    target: Res<LookTarget>,
    mut shown: Local<Option<BlockId>>,
) {
    use slint_ui::slint::{ModelRc, SharedString, VecModel};
    let Some(hud) = hud else { return };
    let ui = hud.overlay.ui();
    let id = target.0.map(|t| t.id);
    // The HUD is made again per session: an empty title there is not ours.
    if *shown == id && (id.is_some() == !ui.get_look_title().is_empty()) {
        return;
    }
    *shown = id;
    match id {
        Some(id) => {
            let (title, lines) = describe_block(id);
            ui.set_look_title(title.into());
            let lines: Vec<SharedString> = lines.into_iter().map(Into::into).collect();
            ui.set_look_lines(ModelRc::new(VecModel::from(lines)));
        }
        None => ui.set_look_title(SharedString::default()),
    }
}
