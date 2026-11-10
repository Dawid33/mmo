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
//! **Stud terrain** has no block textures, so there the plane is coloured from
//! a second texture ([`stud_slice_colors`], `SliceParams::overlay.z`): the LDraw
//! colour of the tile's dominant stud material where the working level's tile
//! and the one above are both `TileFill::Solid` (materialised bricks or implied
//! ground alike — the tile mirror answers both), transparent elsewhere. A cut
//! through anything less than two solid tiles (a floor of plates, a slope, a
//! tree) is left to the stud material's own back-face cap
//! (`stud/material.rs`). Designations are not tinted on it: on studs they are
//! boxes (`stud/overlay.rs`), which stand over the plane.
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
use game::{BlockId, RegionId, TerrainKind, TileFill, TileSource, REGION_BLOCKS, REGION_SIZE};

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

/// The cap's colour on stud terrain where the cut runs through a body of
/// recipe water: the water pass's tint, opaque (sRGB bytes).
pub fn stud_water_cap_rgba() -> [u8; 4] {
    let [r, g, b] = super::water::WATER_COLOR;
    let c = Color::linear_rgb(r, g, b).to_srgba().to_u8_array();
    [c[0], c[1], c[2], 255]
}

/// The cap's colour for stud material `material`: its LDraw colour, opaque
/// (sRGB bytes) — also for a glass material, the cap being a cross-section.
pub fn stud_cap_rgba(material: u16) -> [u8; 4] {
    let assets = super::stud::mesh::stud_assets();
    let rgb = assets.colors.get(&game::stud_material_color(material)).map_or([160, 160, 160], |c| c.rgb);
    [rgb[0], rgb[1], rgb[2], 255]
}

/// [`slice_ids`] for stud terrain: the cap's cells as sRGB RGBA, index
/// `x + z * REGION_BLOCKS` over the home region's TILES (a stud tile is as wide
/// as a block), alpha 0 where there is no cap. A cell is capped where the cut
/// slices through solid ground — the tiles at `level` and `level + 1` both
/// [`TileFill::Solid`] — with the colour of `level`'s dominant material; and,
/// as on voxels, through water: an open tile at `level` holding recipe water
/// under water or a solid tile.
pub fn stud_slice_colors(src: &impl TileSource, level: i32) -> Vec<[u8; 4]> {
    let water = stud_water_cap_rgba();
    (0..REGION_BLOCKS as i32)
        .flat_map(|z| {
            (0..REGION_BLOCKS as i32).map(move |x| {
                let (at, above) = ([x, level, z], [x, level + 1, z]);
                let above_solid = src.tile_fill(above) == TileFill::Solid;
                match src.tile_fill(at) {
                    TileFill::Solid if above_solid => src.tile_material(at).map_or([0; 4], stud_cap_rgba),
                    TileFill::Open if src.water_units(at) > 0 && (above_solid || src.water_units(above) > 0) => water,
                    _ => [0; 4],
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
    /// x: the working level's layer in the overlay texture; y: overlay on (0/1);
    /// z: 1 = stud terrain — colour the cells from `colors`, not `ids`.
    pub overlay: UVec4,
}

/// [`overlay_params`] on stud terrain: the stud colour texture, no overlay
/// tint (stud designations are boxes over the plane).
pub fn stud_overlay_params(level: i32) -> UVec4 {
    UVec4::new(overlay_params(level).x, 0, 1, 0)
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
    /// [`stud_slice_colors`] as an Rgba8UnormSrgb texture: 1 × 1 until the home
    /// region is stud terrain, REGION_BLOCKS² from then on.
    #[texture(6)]
    pub colors: Handle<Image>,
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
    colors: Handle<Image>,
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

/// An Rgba8UnormSrgb 2D texture, kept in the main world too so it can be rewritten.
fn rgba_image(width: u32, height: u32, texels: &[[u8; 4]]) -> Image {
    Image::new(
        Extent3d { width, height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        texels.concat(),
        TextureFormat::Rgba8UnormSrgb,
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
    let colors = images.add(rgba_image(1, 1, &[[0; 4]]));
    let material = materials.add(SlicePlaneMaterial {
        ids: ids.clone(),
        top_layers,
        blocks: blocks.0.clone(),
        params: SliceParams { origin: Vec4::new(0.0, 0.0, 0.0, 1.0 - CAP_DARKEN), overlay: UVec4::ZERO },
        overlay: overlay.0.clone(),
        colors: colors.clone(),
    });
    commands.spawn((
        Name::new("slice plane"),
        SlicePlane { material: material.clone(), ids, colors },
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
    let stud = tiles.terrain() == TerrainKind::Stud;
    if stud {
        // The whole image: its first stud rebuild also grows it from 1 × 1.
        if let Some(mut image) = images.get_mut(&plane.colors) {
            let side = REGION_BLOCKS as u32;
            *image = rgba_image(side, side, &stud_slice_colors(tiles.as_ref(), level));
        }
    } else if let Some(mut image) = images.get_mut(&plane.ids) {
        image.data = Some(bytemuck::cast_slice(&slice_ids(tiles.as_ref(), level)).to_vec());
    }
    if let Some(mut material) = materials.get_mut(&plane.material) {
        material.params.origin = Vec4::new(origin.x, 0.0, origin.z, 1.0 - CAP_DARKEN);
        material.params.overlay = if stud { stud_overlay_params(level) } else { overlay_params(level) };
    }
    *state = SliceState { level: Some(level), region: Some(home), mirror_version: tiles.version() };
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{TilePos, WATER_FULL};

    const STONE: BlockId = BlockId(2);
    const L: i32 = 40;

    const HALF: BlockId = BlockId(7);

    /// `f(x, y, z)` decides the block at each tile.
    struct Stub(fn(i32, i32, i32) -> BlockId);

    impl TileSource for Stub {
        fn block(&self, p: TilePos) -> BlockId {
            (self.0)(p[0], p[1], p[2])
        }
        fn is_bedrock(&self, _: TilePos) -> bool {
            false
        }
        fn is_constructed(&self, _: TilePos) -> bool {
            false
        }
        fn tree_id(&self, _: TilePos) -> u32 {
            0
        }
        fn tree_limb(&self, _: TilePos) -> u8 {
            0
        }
    }

    /// R20: the cap is drawn only where the cut slices through material —
    /// the block at `L` and the one above it both solid. A top open to the
    /// air (full or half block) is left to its own top face.
    #[test]
    fn slice_ids_cap_only_where_the_level_above_is_solid() {
        let ids = slice_ids(
            &Stub(|x, y, _| match (x, y) {
                // x = 0: L solid, L+1 air.
                (0, L) => STONE,
                // x = 1: L and L+1 solid (implied rock through the cut).
                (1, L) | (1, 41) => STONE,
                // x = 2: L air, L+1 solid (a dug room's ceiling).
                (2, 41) => STONE,
                // x = 3: a half block at L, air above.
                (3, L) => HALF,
                _ => BlockId::AIR,
            }),
            L,
        );
        assert_eq!(ids.len(), REGION_BLOCKS * REGION_BLOCKS);
        assert_eq!(ids[0], 0, "L solid, L+1 air: the top face shows");
        assert_eq!(ids[1], STONE.0, "L and L+1 solid: capped with L's block");
        assert_eq!(ids[2], 0, "L air: open to look down into");
        assert_eq!(ids[3], 0, "half block with air above: its own top shows");
        assert_eq!(ids[4], 0);
    }

    /// `Stub` plus recipe water: every non-solid cell at or below `WATER_TOP`.
    struct Lake(fn(i32, i32, i32) -> BlockId);
    const WATER_TOP: i32 = 45;

    impl TileSource for Lake {
        fn block(&self, p: TilePos) -> BlockId {
            (self.0)(p[0], p[1], p[2])
        }
        fn is_bedrock(&self, _: TilePos) -> bool {
            false
        }
        fn is_constructed(&self, _: TilePos) -> bool {
            false
        }
        fn tree_id(&self, _: TilePos) -> u32 {
            0
        }
        fn tree_limb(&self, _: TilePos) -> u8 {
            0
        }
        fn water_units(&self, p: TilePos) -> u16 {
            if p[1] <= WATER_TOP && self.block(p) == BlockId::AIR { WATER_FULL } else { 0 }
        }
    }

    /// A cut below a lake's surface shows its cross-section: water at `L`
    /// under water (or under a solid) is capped with the `water` block; the
    /// lake bed under water keeps its own meshed top face; the water's top
    /// level (air above) is left to the water surface.
    #[test]
    fn slice_ids_cap_water_below_its_surface() {
        let reg = game::BlockRegistry::from_ron(include_str!("../../../../assets/blocks/blocks.ron")).unwrap();
        game::install_block_props(reg.props());
        let water = reg.id_of("water").expect("water block").0;
        let ids = slice_ids(
            &Lake(|x, y, _| match (x, y) {
                // x = 1: the lake bed at L, water above.
                (1, L) => STONE,
                // x = 2: an ice sheet over the water at L.
                (2, 41) => STONE,
                _ => BlockId::AIR,
            }),
            L,
        );
        assert_eq!(ids[0], water, "water under water: the cross-section");
        assert_eq!(ids[1], 0, "the bed's top face shows under the water");
        assert_eq!(ids[2], water, "water under a solid");
        let surface = slice_ids(&Lake(|_, _, _| BlockId::AIR), WATER_TOP);
        assert_eq!(surface[0], 0, "the top water level: the surface shows");
    }

    #[test]
    fn slice_ids_index_is_x_plus_z_times_width() {
        let ids = slice_ids(&Stub(|x, y, z| if x == 3 && z == 5 && (y == L || y == L + 1) { STONE } else { BlockId::AIR }), L);
        let at = 3 + 5 * REGION_BLOCKS;
        assert_eq!(ids[at], STONE.0);
        assert_eq!(ids.iter().filter(|&&id| id != 0).count(), 1);
    }

    #[test]
    fn rebuild_needed_on_level_region_or_version_change() {
        let home = Some(RegionId { x: 0, z: 0 });
        let state = SliceState { level: Some(10), region: home, mirror_version: 7 };
        assert!(!needs_rebuild(&state, Some(10), home, 7));
        assert!(needs_rebuild(&state, Some(11), home, 7));
        assert!(needs_rebuild(&state, Some(10), Some(RegionId { x: 1, z: 0 }), 7));
        assert!(needs_rebuild(&state, Some(10), home, 8));
        assert!(needs_rebuild(&SliceState::default(), Some(10), home, 0));
    }

    /// The plane's cells are the working level's: the overlay's top layer.
    #[test]
    fn overlay_params_pick_the_working_level_layer() {
        use crate::renderer::fortress_view::OVERLAY_LEVELS;
        assert_eq!(overlay_params(L), UVec4::new(OVERLAY_LEVELS - 1, 1, 0, 0));
        assert_eq!(overlay_params(-3), UVec4::new(OVERLAY_LEVELS - 1, 1, 0, 0));
    }

    /// A stud tile source: `f(x, y, z)` gives each tile's fill and material.
    struct StudStub(fn(i32, i32, i32) -> (TileFill, u16), i32);

    impl TileSource for StudStub {
        fn block(&self, _: TilePos) -> BlockId {
            BlockId::AIR
        }
        fn terrain(&self) -> TerrainKind {
            TerrainKind::Stud
        }
        fn tile_fill(&self, p: TilePos) -> TileFill {
            (self.0)(p[0], p[1], p[2]).0
        }
        fn tile_material(&self, p: TilePos) -> Option<u16> {
            let (fill, m) = (self.0)(p[0], p[1], p[2]);
            (fill != TileFill::Open).then_some(m)
        }
        fn is_bedrock(&self, _: TilePos) -> bool {
            false
        }
        fn is_constructed(&self, _: TilePos) -> bool {
            false
        }
        fn tree_id(&self, _: TilePos) -> u32 {
            0
        }
        fn tree_limb(&self, _: TilePos) -> u8 {
            0
        }
        /// Water in every open tile at or under level `self.1`.
        fn water_units(&self, p: TilePos) -> u16 {
            if p[1] <= self.1 && self.tile_fill(p) == TileFill::Open { WATER_FULL } else { 0 }
        }
    }

    /// On studs the cap is a colour per tile: the dominant material's LDraw
    /// colour where the cut slices two solid tiles, nothing where the tile
    /// above is a floor or open (its own bricks show, or the stud material's
    /// back-face cap), water under water.
    #[test]
    fn stud_slice_colors_cap_solid_under_solid_in_the_materials_colour() {
        const DRY: i32 = i32::MIN;
        let src = StudStub(
            |x, y, _| match (x, y) {
                // x = 0: solid stone under solid dirt.
                (0, L) => (TileFill::Solid, game::MAT_STONE),
                (0, 41) => (TileFill::Solid, game::MAT_DIRT),
                // x = 1: solid under a floor of plates.
                (1, L) => (TileFill::Solid, game::MAT_STONE),
                (1, 41) => (TileFill::Floor(2), game::MAT_DIRT),
                // x = 2: a floor under solid (a dug room's ceiling over a low floor).
                (2, L) => (TileFill::Floor(3), game::MAT_STONE),
                (2, 41) => (TileFill::Solid, game::MAT_STONE),
                // x = 3: solid, open above.
                (3, L) => (TileFill::Solid, game::MAT_GRASS),
                // x = 4: red plastic under plastic.
                (4, L) | (4, 41) => (TileFill::Solid, 4),
                _ => (TileFill::Open, 0),
            },
            DRY,
        );
        let colors = stud_slice_colors(&src, L);
        assert_eq!(colors.len(), REGION_BLOCKS * REGION_BLOCKS);
        let assets = crate::renderer::stud::mesh::stud_assets();
        let rgb = |m: u16| assets.colors[&game::stud_material_color(m)].rgb;
        let stone = rgb(game::MAT_STONE);
        assert_eq!(colors[0], [stone[0], stone[1], stone[2], 255], "capped with the working level's material");
        assert_eq!(colors[1], [0; 4], "plates above: no plane");
        assert_eq!(colors[2], [0; 4], "a partial tile at the level: no plane");
        assert_eq!(colors[3], [0; 4], "open above: the top shows");
        let red = rgb(4);
        assert_eq!(colors[4], [red[0], red[1], red[2], 255], "a plastic by its colour code");
        assert_eq!(colors[5], [0; 4], "air");
        assert_ne!(colors[0], colors[4]);
        // The index is x + z * width.
        let one = StudStub(|x, y, z| if (x, z) == (3, 5) && (y == L || y == L + 1) { (TileFill::Solid, game::MAT_STONE) } else { (TileFill::Open, 0) }, DRY);
        let colors = stud_slice_colors(&one, L);
        assert_ne!(colors[3 + 5 * REGION_BLOCKS], [0; 4]);
        assert_eq!(colors.iter().filter(|c| c[3] != 0).count(), 1);
    }

    #[test]
    fn stud_slice_colors_cap_water_below_its_surface() {
        const TOP: i32 = 45;
        let lake = StudStub(
            |x, y, _| match (x, y) {
                (1, L) => (TileFill::Solid, game::MAT_SAND),
                (2, 41) => (TileFill::Solid, game::MAT_ICE),
                _ => (TileFill::Open, 0),
            },
            TOP,
        );
        let water = stud_water_cap_rgba();
        assert_eq!(water[3], 255);
        let colors = stud_slice_colors(&lake, L);
        assert_eq!(colors[0], water, "water under water: the cross-section");
        assert_eq!(colors[1], [0; 4], "the bed's top shows under the water");
        assert_eq!(colors[2], water, "water under a solid");
        assert_eq!(stud_slice_colors(&lake, TOP)[0], [0; 4], "the top water level: the surface shows");
    }

    #[test]
    fn stud_params_switch_the_plane_to_colours_without_the_overlay_tint() {
        use crate::renderer::fortress_view::OVERLAY_LEVELS;
        assert_eq!(stud_overlay_params(L), UVec4::new(OVERLAY_LEVELS - 1, 0, 1, 0));
        assert_eq!(overlay_params(L).z, 0, "voxel terrain keeps the block textures");
    }

    #[test]
    fn top_layer_lut_maps_ids_and_defaults_to_missing() {
        let layers = crate::renderer::block_textures::BlockTextureLayers(
            [(STONE, [5, 6, 7]), (BlockId(4), [9, 9, 9])].into_iter().collect(),
        );
        assert_eq!(top_layer_lut(&layers), vec![0, 0, 5, 0, 9]);
    }

    #[test]
    fn slice_plane_shader_is_valid_wgsl() {
        use crate::renderer::far_voxel::pipeline::tests::validate_shader;
        use crate::renderer::voxel_pull::pipeline::tests::IMPORTS;
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/shaders/slice_plane.wgsl"
        ))
        .expect("slice_plane.wgsl");
        let forward_io = (
            "forward_io.wgsl",
            r#"
#define_import_path bevy_pbr::forward_io
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
}
"#,
        );
        assert!(
            source.contains("@binding(5) var overlay: texture_3d<u32>;"),
            "the slice plane reads the designation overlay"
        );
        assert!(
            source.contains("@binding(6) var colors: texture_2d<f32>;"),
            "the slice plane reads the stud cap colours"
        );
        let imports: Vec<(&str, &str)> = IMPORTS.into_iter().chain([forward_io]).collect();
        for fog in [false, true] {
            let mut defs = vec![bevy::shader::ShaderDefVal::UInt("MATERIAL_BIND_GROUP".into(), 3)];
            if fog {
                defs.push("DISTANCE_FOG".into());
            }
            validate_shader(source.clone(), &defs, &imports)
                .unwrap_or_else(|e| panic!("slice_plane.wgsl with {defs:?}: {e:?}"));
        }
    }
}
