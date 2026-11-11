//! Slice plane: the cut's cap for IMPLIED ground. Below a column's
//! materialised chunks the ground exists only as a per-column recipe (mirrored
//! by the commander's `TileMirror`) and has no mesh, so the cutaway's
//! back-face cap (`stud/material.rs`) finds nothing to draw there and the cut
//! would show a void where rock is. This plane fills it: one `REGION_SIZE`
//! square lying exactly on the cut plane over the home region, coloured per
//! tile ([`stud_slice_colors`]) with the LDraw colour of the tile's dominant
//! stud material where the working level's tile and the one above are both
//! `TileFill::Solid` (materialised bricks or implied ground alike — the tile
//! mirror answers both), darkened like the cap ([`CAP_DARKEN`]); everywhere
//! else it discards, so dug spaces stay open to look down into. A cut through
//! anything less than two solid tiles (a floor of plates, a slope, a tree) is
//! left to the stud material's own back-face cap. Water is capped too: the
//! water pass discards everything above the cut, so a cut through a lake or
//! the sea shows its cross-section here.
//!
//! Depth: the plane is at the cut. The back-face cap lies deeper (the solid's
//! exit face), and solid below the cut is under it, so the plane wins there
//! with no z-fight.
//!
//! Designations are not tinted on it: they are boxes (`stud/overlay.rs`),
//! which stand over the plane.
//!
//! The colour texture is rebuilt (whole) only when the level, the home region
//! or the mirror changes ([`needs_rebuild`]). Commander mode only; hidden
//! whenever the cut is off (`camera_cut` is `None`). No prepass, casts no
//! shadows — the solid it stands for already casts them through its meshed
//! faces.

use bevy::{
    asset::RenderAssetUsages,
    pbr::{Material, MaterialPlugin},
    prelude::*,
    reflect::TypePath,
    render::render_resource::{AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat},
    shader::ShaderRef,
};
use game::{RegionId, TileFill, TileSource, REGION_BLOCKS, REGION_SIZE};

use super::commander::level::{camera_cut, WorkingLevel};
use super::commander::tiles::TileMirror;
use super::fortress_view::CAP_DARKEN;

/// The cap's colour where the cut runs through a body of
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

/// The cap's cells as sRGB RGBA, index `x + z * REGION_BLOCKS` over the home
/// region's TILES (a tile is a metre wide), alpha 0 where there is no cap. A
/// cell is capped where the cut slices through solid ground — the tiles at
/// `level` and `level + 1` both [`TileFill::Solid`] — with the colour of
/// `level`'s dominant material; and through water: an open tile at `level`
/// holding recipe water under water or a solid tile.
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

/// What the colour texture was last built from.
#[derive(Resource, Default)]
pub struct SliceState {
    pub level: Option<i32>,
    pub region: Option<RegionId>,
    pub mirror_version: u64,
}

/// Whether the colour texture is stale for this level, region and mirror version.
pub fn needs_rebuild(state: &SliceState, level: Option<i32>, region: Option<RegionId>, version: u64) -> bool {
    state.level != level || state.region != region || state.mirror_version != version
}

/// Mirrored by `SliceParams` in `slice_plane.wgsl`.
#[derive(Clone, Copy, Default, ShaderType)]
pub struct SliceParams {
    /// xz: home region world origin; y: unused; w: brightness (1 - CAP_DARKEN).
    pub origin: Vec4,
}

#[derive(Asset, AsBindGroup, TypePath, Clone)]
pub struct SlicePlaneMaterial {
    #[uniform(0)]
    pub params: SliceParams,
    /// [`stud_slice_colors`] as an Rgba8UnormSrgb REGION_BLOCKS² texture.
    #[texture(1)]
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

/// Spawn the (hidden) plane once.
fn spawn_slice_plane(
    mut commands: Commands,
    existing: Query<(), With<SlicePlane>>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<SlicePlaneMaterial>>,
) {
    if !existing.is_empty() {
        return;
    }
    let side = REGION_BLOCKS as u32;
    let colors = images.add(rgba_image(side, side, &vec![[0; 4]; (side * side) as usize]));
    let material = materials.add(SlicePlaneMaterial {
        params: SliceParams { origin: Vec4::new(0.0, 0.0, 0.0, 1.0 - CAP_DARKEN) },
        colors: colors.clone(),
    });
    commands.spawn((
        Name::new("slice plane"),
        SlicePlane { material: material.clone(), colors },
        Mesh3d(meshes.add(Plane3d::new(Vec3::Y, Vec2::splat(REGION_SIZE / 2.0)).mesh())),
        MeshMaterial3d(material),
        Transform::default(),
        Visibility::Hidden,
    ));
}

/// Follow the cut: place the plane on it over the home region, hide it while
/// the cut is off, and rebuild the colour texture when stale.
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
    if let Some(mut image) = images.get_mut(&plane.colors) {
        image.data = Some(stud_slice_colors(tiles.as_ref(), level).concat());
    }
    if let Some(mut material) = materials.get_mut(&plane.material) {
        material.params.origin = Vec4::new(origin.x, 0.0, origin.z, 1.0 - CAP_DARKEN);
    }
    *state = SliceState { level: Some(level), region: Some(home), mirror_version: tiles.version() };
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{TilePos, WATER_FULL};

    const L: i32 = 40;

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

    /// A stud tile source: `f(x, y, z)` gives each tile's fill and material.
    struct StudStub(fn(i32, i32, i32) -> (TileFill, u16), i32);

    impl TileSource for StudStub {
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
    fn slice_plane_shader_is_valid_wgsl() {
        use crate::renderer::shader_test::{validate_shader, IMPORTS};
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
        assert!(source.contains("@binding(0) var<uniform> params: SliceParams;"));
        assert!(
            source.contains("@binding(1) var colors: texture_2d<f32>;"),
            "the slice plane reads the cap colours"
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
