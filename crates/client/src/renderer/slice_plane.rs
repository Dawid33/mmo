//! Slice plane: the cut's cap for IMPLIED ground. Below a column's
//! materialised surface band the ground exists only as a per-column recipe
//! (`GameData.ground`, mirrored by the commander's `TileMirror`) and has no
//! chunk mesh, so the cutaway's back-face cap (`fortress_view`,
//! `voxel_pull.wgsl`) finds nothing to draw there and the cut would show a
//! void where rock is. This plane fills it: one `REGION_SIZE` square lying
//! exactly on the cut plane over the home region, coloured per block cell by
//! the top texture of the block the mirror reports at the working level
//! (materialised or implied), darkened like the cap ([`CAP_DARKEN`]). It is
//! drawn only where the cut slices through material — the working level AND
//! the level above both solid ([`slice_ids`]); everywhere else it discards,
//! so dug spaces stay open to look down into and a working-level block open
//! to the air shows its own top face (a half block at its true height),
//! undarkened.
//!
//! Depth: the plane is at the cut. The back-face cap lies deeper (the solid's
//! exit face), and solid below the cut is under it, so the plane wins there
//! with no z-fight. No top face lies coplanar with it: a +Y face on the cut
//! exists only where the level above is air (or not a full block), exactly
//! where the plane discards. Ceilings (-Y faces of blocks above the cut)
//! lying on the plane are collapsed by the cutaway shader, in every pass:
//! they belong to cut-away rock, and drawn as back-face caps they would roof
//! over every dug room the plane leaves open.
//!
//! The designation overlay tints it like the cutaway's cap: the plane reads
//! the same overlay texture (`fortress_view::OverlayTexture`) at the working
//! level's layer, since designated implied rock at the cut has no face.
//!
//! The ids texture is rebuilt (whole: 128 KiB at 256 m regions) only when the
//! level, the home region or the mirror changes ([`needs_rebuild`]). Commander
//! mode only; hidden whenever the cut is off (`camera_cut` is `None`). The
//! per-chunk Mesh fallback (WebGL2) caps with it too — its cutaway lives in
//! `voxel_texture.wgsl`. No prepass, casts no shadows — the solid it stands for
//! already casts them through its meshed faces.

use bevy::{
    asset::RenderAssetUsages,
    pbr::{Material, MaterialPlugin},
    prelude::*,
    reflect::TypePath,
    render::render_resource::{AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat},
    shader::ShaderRef,
};
use game::{BlockId, RegionId, TileSource, REGION_BLOCKS, REGION_SIZE};

use super::block_textures::BlockTextureLayers;
use super::commander::level::{camera_cut, WorkingLevel};
use super::commander::tiles::TileMirror;
use super::fortress_view::{overlay_base_level, OverlayTexture, CAP_DARKEN};
use super::voxel_pull::BlockArrayTexture;

/// The cap's cells across the home region: `REGION_BLOCKS²` cells, index
/// `x + z * REGION_BLOCKS`, `BlockId` as u16. A cell holds the id of the
/// block at `level` only where the cut slices through material — the blocks
/// at `level` and `level + 1` both solid (materialised or implied) — and
/// `AIR` (0, discarded) everywhere else: over air at `level`, and over a
/// block open to the level above, whose own top face (full or half block)
/// then shows at its true height, undarkened.
///
/// Water ([`TileSource::water_units`] > 0) is filled too: the water pass
/// discards everything above the cut, so a cut through a lake or the sea
/// shows its cross-section here, capped with the manifest's `water` block —
/// where the water at `level` has water or a solid above it. A solid at
/// `level` under water is not capped: its top face is meshed (water is not
/// in the chunks) and lies on the cut, as under air.
pub fn slice_ids(src: &impl TileSource, level: i32) -> Vec<u16> {
    let props = game::block_props();
    let water = props.id_named("water").unwrap_or(BlockId::AIR);
    (0..REGION_BLOCKS as i32)
        .flat_map(|z| {
            (0..REGION_BLOCKS as i32).map(move |x| {
                let below = src.block([x, level, z]);
                let above = src.block([x, level + 1, z]);
                let above_solid = props.blocks_ray(above);
                if props.blocks_ray(below) {
                    if above_solid { below.0 } else { 0 }
                } else if src.water_units([x, level, z]) > 0
                    && (above_solid || src.water_units([x, level + 1, z]) > 0)
                {
                    water.0
                } else {
                    0
                }
            })
        })
        .collect()
}

/// `BlockId` -> its top-face texture layer, 0 (the missing texture) for ids
/// without layers. Indexed by id.
pub fn top_layer_lut(layers: &BlockTextureLayers) -> Vec<u16> {
    let len = layers.0.keys().map(|id| id.0 as usize + 1).max().unwrap_or(1);
    let mut lut = vec![0u16; len];
    for (id, [top, _, _]) in &layers.0 {
        lut[id.0 as usize] = *top as u16;
    }
    lut
}

/// What the ids texture was last built from.
#[derive(Resource, Default)]
pub struct SliceState {
    pub level: Option<i32>,
    pub region: Option<RegionId>,
    pub mirror_version: u64,
}

/// Whether the ids texture is stale for this level, region and mirror version.
pub fn needs_rebuild(state: &SliceState, level: Option<i32>, region: Option<RegionId>, version: u64) -> bool {
    state.level != level || state.region != region || state.mirror_version != version
}

/// Mirrored by `SliceParams` in `slice_plane.wgsl`.
#[derive(Clone, Copy, Default, ShaderType)]
pub struct SliceParams {
    /// xz: home region world origin; y: unused; w: brightness (1 - CAP_DARKEN).
    pub origin: Vec4,
    /// x: the working level's layer in the overlay texture; y: overlay on (0/1).
    pub overlay: UVec4,
}

/// The overlay params for working level `level`: its layer, on.
pub fn overlay_params(level: i32) -> UVec4 {
    UVec4::new((level - overlay_base_level(level)) as u32, 1, 0, 0)
}

#[derive(Asset, AsBindGroup, TypePath, Clone)]
pub struct SlicePlaneMaterial {
    /// `slice_ids` as an R16Uint REGION_BLOCKS² texture.
    #[texture(0, sample_type = "u_int")]
    pub ids: Handle<Image>,
    /// `top_layer_lut` as an R16Uint (len × 1) texture.
    #[texture(1, sample_type = "u_int")]
    pub top_layers: Handle<Image>,
    /// The block array texture the terrain draws with.
    #[texture(2, dimension = "2d_array")]
    #[sampler(3)]
    pub blocks: Handle<Image>,
    #[uniform(4)]
    pub params: SliceParams,
    /// The designation overlay (`fortress_view::OverlayTexture`), R8Uint 3D.
    #[texture(5, sample_type = "u_int", dimension = "3d")]
    pub overlay: Handle<Image>,
}

impl Material for SlicePlaneMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/slice_plane.wgsl".into()
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }
}

/// The plane entity and the handles it rebuilds.
#[derive(Component)]
pub struct SlicePlane {
    material: Handle<SlicePlaneMaterial>,
    ids: Handle<Image>,
}

pub struct SlicePlanePlugin;

impl Plugin for SlicePlanePlugin {
    fn build(&self, app: &mut App) {
        use crate::session::SessionResetExt;
        app.add_plugins(MaterialPlugin::<SlicePlaneMaterial>::default())
            .add_session_reset::<SliceState>()
            .add_systems(
                Update,
                (spawn_slice_plane, update_slice_plane)
                    .chain()
                    .after(super::commander::level::track_camera_level)
                    .after(super::commander::tiles::update_tile_mirror)
                    .run_if(crate::session::world_systems_enabled()),
            );
    }
}

/// An R16Uint 2D texture, kept in the main world too so it can be rewritten.
fn u16_image(width: u32, height: u32, texels: &[u16]) -> Image {
    Image::new(
        Extent3d { width, height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        bytemuck::cast_slice(texels).to_vec(),
        TextureFormat::R16Uint,
        RenderAssetUsages::default(),
    )
}

/// Spawn the (hidden) plane once the block textures exist.
fn spawn_slice_plane(
    mut commands: Commands,
    existing: Query<(), With<SlicePlane>>,
    blocks: Option<Res<BlockArrayTexture>>,
    layers: Option<Res<BlockTextureLayers>>,
    overlay: Option<Res<OverlayTexture>>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<SlicePlaneMaterial>>,
) {
    let (Some(blocks), Some(layers), Some(overlay)) = (blocks, layers, overlay) else { return };
    if !existing.is_empty() {
        return;
    }
    let side = REGION_BLOCKS as u32;
    let ids = images.add(u16_image(side, side, &vec![0; (side * side) as usize]));
    let lut = top_layer_lut(&layers);
    let top_layers = images.add(u16_image(lut.len() as u32, 1, &lut));
    let material = materials.add(SlicePlaneMaterial {
        ids: ids.clone(),
        top_layers,
        blocks: blocks.0.clone(),
        params: SliceParams { origin: Vec4::new(0.0, 0.0, 0.0, 1.0 - CAP_DARKEN), overlay: UVec4::ZERO },
        overlay: overlay.0.clone(),
    });
    commands.spawn((
        Name::new("slice plane"),
        SlicePlane { material: material.clone(), ids },
        Mesh3d(meshes.add(Plane3d::new(Vec3::Y, Vec2::splat(REGION_SIZE / 2.0)).mesh())),
        MeshMaterial3d(material),
        Transform::default(),
        Visibility::Hidden,
    ));
}

/// Follow the cut: place the plane on it over the home region, hide it while
/// the cut is off, and rebuild the ids texture when stale.
fn update_slice_plane(
    working: Res<WorkingLevel>,
    tiles: Res<TileMirror>,
    mut state: ResMut<SliceState>,
    mut plane: Query<(&SlicePlane, &mut Transform, &mut Visibility)>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<SlicePlaneMaterial>>,
) {
    let Ok((plane, mut tf, mut visibility)) = plane.single_mut() else { return };
    let (Some(cut), Some(home), Some(level)) = (camera_cut(&working, &tiles), tiles.region(), working.level) else {
        visibility.set_if_neq(Visibility::Hidden);
        return;
    };
    let origin = Vec3::from_array(home.world_offset());
    let half = REGION_SIZE / 2.0;
    tf.set_if_neq(Transform::from_xyz(origin.x + half, cut, origin.z + half));
    visibility.set_if_neq(Visibility::Inherited);
    if !needs_rebuild(&state, Some(level), Some(home), tiles.version()) {
        return;
    }
    if let Some(mut image) = images.get_mut(&plane.ids) {
        image.data = Some(bytemuck::cast_slice(&slice_ids(tiles.as_ref(), level)).to_vec());
    }
    if let Some(mut material) = materials.get_mut(&plane.material) {
        material.params.origin = Vec4::new(origin.x, 0.0, origin.z, 1.0 - CAP_DARKEN);
        material.params.overlay = overlay_params(level);
    }
    *state = SliceState { level: Some(level), region: Some(home), mirror_version: tiles.version() };
}

