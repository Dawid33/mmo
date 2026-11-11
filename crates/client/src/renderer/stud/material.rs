//! The stud terrain material: Bevy's `StandardMaterial` (vertex colours carry
//! the brick colours) extended with the fortress view, so the commander
//! camera's z-level cut, its cap and the darkening of the levels below apply to
//! stud chunks, stud tops and falling stud bodies.
//!
//! The extension reads the [`FortressViewUniform`] (also bound by the water
//! material), copied in by [`sync_stud_materials`]; its cut plane is a TILE
//! boundary of the home terrain (`commander::level::cut_y` goes through
//! `game::tiles`). The level height and the depth-darkening curve travel in
//! [`StudFortressParams`], built from the Rust consts ([`fortress_params`]), so
//! `assets/shaders/stud_terrain.wgsl` restates no number.
//!
//! Three materials per session ([`StudMaterials`]):
//! - **outside** — everything that never lies in the home region's rect: the
//!   chunks of the other loaded regions and the far tiles. The cut, its cap and
//!   the darkening apply inside that rect only, so this one never carries the
//!   cut: always plain opaque, back-face culled, no fragment work in the depth
//!   and shadow passes — whatever the cut does. (Sharing the cut material made
//!   every loaded region and all ~1 200 far tiles alpha-masked and two-sided
//!   whenever the cut was on.)
//! - **opaque** — the home region's chunk bodies and detail, and falling bodies
//!   (they move; the cut is by world position). While the cut is on
//!   it is alpha-masked (so the depth / shadow prepass runs the shader's
//!   discard: cut-away bricks cast no shadow) and unculled (a back face inside
//!   the home region is the cut's cap). With the cut off it is plain opaque,
//!   back-face culled — as before this material existed.
//! - **glass** — bricks of a transparent LDraw colour ([`is_transparent`] in
//!   `mesh.rs`): alpha-blended, cut away above the plane, darkened below, no cap.
//!
//! All of them also paint the home region's boundary line and dim what lies outside
//! it (`boundaries.rs`' [`BoundaryUniform`]).
//!
//! Works on WebGL2: three small uniform buffers beside the standard material's.
//! Designations are not drawn here: they are boxes (`overlay.rs`).

use bevy::{
    mesh::MeshVertexBufferLayoutRef,
    pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline},
    prelude::*,
    reflect::TypePath,
    render::render_resource::{AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError},
    shader::ShaderRef,
};
use game::{RegionId};

use super::mesh::STUD_EDGE_ALPHA;
use crate::renderer::boundaries::BoundaryUniform;
use crate::renderer::fortress_view::{cutaway_active, FortressViewUniform, DEPTH_DARKEN_FLOOR, DEPTH_DARKEN_PER_LEVEL};

/// The material stud meshes draw with.
pub type StudTerrainMaterial = ExtendedMaterial<StandardMaterial, StudFortressExt>;

/// Perceptual roughness of the plastic.
pub const STUD_ROUGHNESS: f32 = 0.35;
/// Alpha cutoff of the opaque material while it is masked (the cut is on).
/// Every opaque vertex alpha ([`STUD_EDGE_ALPHA`] included) is above it: the
/// mask exists only to make the prepass run the cut's discard.
pub const STUD_MASK_CUTOFF: f32 = 0.5;
/// A vertex alpha under this is a seam line ([`STUD_EDGE_ALPHA`]), half-way
/// between it and a face's 1.
pub const STUD_SEAM_ALPHA_BELOW: f32 = (1.0 + STUD_EDGE_ALPHA) / 2.0;

/// `assets/shaders/stud_terrain.wgsl`, compiled into the binary instead of
/// loaded from `assets/` (no hot reload): its uniform layouts change with the
/// Rust that fills them, and a browser serves a cached `.wgsl` across rebuilds
/// (the dev server and the deployed file server send no cache headers).
/// Embedded, the shader always matches its Rust side.
pub const STUD_TERRAIN_SHADER: Handle<Shader> = bevy::asset::uuid_handle!("7c2e41b8-9d53-4a06-b1f7-3e8a5d0c6f92");

/// Register [`STUD_TERRAIN_SHADER`] (before the material is used).
pub fn embed_stud_terrain_shader(app: &mut App) {
    bevy::asset::load_internal_asset!(
        app,
        STUD_TERRAIN_SHADER,
        "../../../../../assets/shaders/stud_terrain.wgsl",
        Shader::from_wgsl
    );
}

/// Mirrored by `StudFortressParams` in `stud_terrain.wgsl`.
#[derive(Clone, Copy, PartialEq, Debug, Default, ShaderType)]
pub struct StudFortressParams {
    /// x: a level's height (one tile), world units; y: brightness lost per
    /// level below the cut; z: the darkest a level gets; w: unused.
    pub levels: Vec4,
    /// x: 1 = back faces are the cut's cap, 0 = none; y: the vertex alpha under
    /// which a fragment is a seam line (never a cap); zw: unused.
    pub flags: Vec4,
}

/// The params for home terrain `kind`: its level height and the voxel
/// shaders' depth-darkening constants. `caps`: the opaque material's.
pub fn fortress_params(caps: bool) -> StudFortressParams {
    StudFortressParams {
        levels: Vec4::new(game::tile_height_units(), DEPTH_DARKEN_PER_LEVEL, DEPTH_DARKEN_FLOOR, 0.0),
        flags: Vec4::new(if caps { 1.0 } else { 0.0 }, STUD_SEAM_ALPHA_BELOW, 0.0, 0.0),
    }
}

/// The shader's `depth_darkening`: the brightness of a face at world height
/// `y` under the cut plane `cut` — full on the working level (the one whose
/// top is the cut), [`DEPTH_DARKEN_PER_LEVEL`] less per level down, never
/// under [`DEPTH_DARKEN_FLOOR`]. The Rust twin of the WGSL, for tests and
/// for anything CPU-side that must match.
pub fn depth_darkening(params: &StudFortressParams, y: f32, cut: f32) -> f32 {
    let levels_below = ((cut - y) / params.levels.x - 1.0 - 1e-3).ceil().max(0.0);
    (1.0 - params.levels.y * levels_below).max(params.levels.z)
}

/// Whether a fragment at world `p` is cut away under `u` (the shader's
/// `cut_away`): inside the home region's rect, above the plane.
pub fn cut_away(u: &FortressViewUniform, p: Vec3) -> bool {
    const CUT_EPSILON: f32 = 0.01;
    let (lo, hi) = (u.origin.xz(), u.origin.xz() + Vec2::splat(u.cut.w));
    cutaway_active(u) && p.x >= lo.x && p.z >= lo.y && p.x < hi.x && p.z < hi.y && p.y > u.cut.x + CUT_EPSILON
}

/// The fortress view as `stud_terrain.wgsl` reads it (bindings 100 – 102).
#[derive(Asset, AsBindGroup, Clone, TypePath)]
#[bind_group_data(StudFortressKey)]
pub struct StudFortressExt {
    #[uniform(100)]
    pub fortress: FortressViewUniform,
    #[uniform(101)]
    pub params: StudFortressParams,
    /// The home region's boundary line (`boundaries.rs`); off by default.
    #[uniform(102)]
    pub boundaries: BoundaryUniform,
}

/// Pipeline key: while the cut is on, the capping material draws back faces
/// (the cut's cap inside the home region; the shader drops them elsewhere).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StudFortressKey {
    pub uncull: bool,
}

impl From<&StudFortressExt> for StudFortressKey {
    fn from(m: &StudFortressExt) -> Self {
        Self { uncull: cutaway_active(&m.fortress) && m.params.flags.x != 0.0 }
    }
}

impl MaterialExtension for StudFortressExt {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(STUD_TERRAIN_SHADER)
    }

    // Runs for the opaque material while it is masked (the cut is on) and
    // wherever Bevy needs a prepass fragment anyway; blended glass has no prepass.
    fn prepass_fragment_shader() -> ShaderRef {
        ShaderRef::Handle(STUD_TERRAIN_SHADER)
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if key.bind_group_data.uncull {
            descriptor.primitive.cull_mode = None;
        }
        Ok(())
    }
}

/// The alpha mode of the opaque material: masked while the cut is on (see the
/// module docs), else opaque.
pub fn opaque_alpha_mode(u: &FortressViewUniform) -> AlphaMode {
    if cutaway_active(u) { AlphaMode::Mask(STUD_MASK_CUTOFF) } else { AlphaMode::Opaque }
}

/// A fresh opaque stud material for fortress view `u` on home terrain `kind`.
pub fn opaque_material(u: FortressViewUniform) -> StudTerrainMaterial {
    ExtendedMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: STUD_ROUGHNESS,
            alpha_mode: opaque_alpha_mode(&u),
            ..default()
        },
        extension: StudFortressExt { fortress: u, params: fortress_params(true), boundaries: default() },
    }
}

/// A fresh material for stud meshes that never lie in the home region's rect: the
/// opaque one with the cut off, for good (see the module docs).
pub fn outside_material() -> StudTerrainMaterial {
    let mut m = opaque_material(FortressViewUniform::default());
    m.extension.params = fortress_params(false);
    m
}

/// A fresh glass material: blended, its alpha from the vertex colours.
pub fn glass_material(u: FortressViewUniform) -> StudTerrainMaterial {
    ExtendedMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: STUD_ROUGHNESS,
            alpha_mode: AlphaMode::Blend,
            ..default()
        },
        extension: StudFortressExt { fortress: u, params: fortress_params(false), boundaries: default() },
    }
}

/// The materials every stud mesh shares, made on first use, once per session.
#[derive(Resource, Default)]
pub struct StudMaterials {
    opaque: Option<Handle<StudTerrainMaterial>>,
    glass: Option<Handle<StudTerrainMaterial>>,
    outside: Option<Handle<StudTerrainMaterial>>,
    /// The home region the chunk materials were last handed out for
    /// ([`retarget_stud_materials`]).
    home: Option<RegionId>,
}

impl StudMaterials {
    /// The opaque material of a mesh that never lies in the home region's rect
    /// (another region's chunks, far tiles): never cut.
    pub fn outside(&mut self, materials: &mut Assets<StudTerrainMaterial>) -> Handle<StudTerrainMaterial> {
        self.outside.get_or_insert_with(|| materials.add(outside_material())).clone()
    }

    /// The opaque material for a chunk of `region`: the cut one in the home
    /// region, else [`Self::outside`].
    pub fn opaque_in(&mut self, region: RegionId, materials: &mut Assets<StudTerrainMaterial>) -> Handle<StudTerrainMaterial> {
        if self.home == Some(region) { self.opaque(materials) } else { self.outside(materials) }
    }

    /// The opaque material that takes the cut (the home region's chunks, falling bodies).
    pub fn opaque(&mut self, materials: &mut Assets<StudTerrainMaterial>) -> Handle<StudTerrainMaterial> {
        self.opaque
            .get_or_insert_with(|| materials.add(opaque_material(FortressViewUniform::default())))
            .clone()
    }

    /// The glass material (bricks of a transparent colour).
    pub fn glass(&mut self, materials: &mut Assets<StudTerrainMaterial>) -> Handle<StudTerrainMaterial> {
        self.glass
            .get_or_insert_with(|| materials.add(glass_material(FortressViewUniform::default())))
            .clone()
    }
}

/// Copy the fortress view, the boundary overlay and the home terrain's params into the stud
/// materials when they change, and switch the opaque material's
/// alpha mode with the cut. A material made after the last change is brought
/// in step here too (it is created with the default view).
pub fn sync_stud_materials(
    uniform: Option<Res<FortressViewUniform>>,
    boundaries: Option<Res<BoundaryUniform>>,
    handles: Res<StudMaterials>,
    materials: Option<ResMut<Assets<StudTerrainMaterial>>>,
) {
    let (Some(uniform), Some(mut materials)) = (uniform, materials) else { return };
    let boundaries = boundaries.map(|b| *b).unwrap_or_default();
    let off = FortressViewUniform::default();
    let entries = [
        (handles.opaque.as_ref(), true, *uniform, opaque_alpha_mode(&uniform)),
        (handles.glass.as_ref(), false, *uniform, AlphaMode::Blend),
        // Never cut: only the boundary line and the terrain's params reach it.
        (handles.outside.as_ref(), false, off, AlphaMode::Opaque),
    ];
    for (handle, caps, view, alpha) in entries {
        let Some(handle) = handle else { continue };
        let params = fortress_params(caps);
        let stale = materials.get(handle).is_some_and(|m| {
            m.extension.fortress != view
                || m.extension.params != params
                || m.extension.boundaries != boundaries
                || m.base.alpha_mode != alpha
        });
        if stale {
            if let Some(mut m) = materials.get_mut(handle) {
                m.extension.fortress = view;
                m.extension.params = params;
                m.extension.boundaries = boundaries;
                m.base.alpha_mode = alpha;
            }
        }
    }
}

/// Hand the home region's chunks the cut material and every other region's the
/// uncut one, when the home region changes (chunks meshed later get theirs in
/// `systems::apply_stud_meshes`, through [`StudMaterials::opaque_in`]). Walks the
/// chunks only on that change.
pub fn retarget_stud_materials(
    tiles: Option<Res<crate::renderer::commander::tiles::TileMirror>>,
    mut handles: ResMut<StudMaterials>,
    materials: Option<ResMut<Assets<StudTerrainMaterial>>>,
    chunks: Query<(&crate::renderer::bridge::SimEntity, &super::systems::StudMeshChildren)>,
    mut drawn: Query<&mut MeshMaterial3d<StudTerrainMaterial>>,
) {
    let home = tiles.and_then(|t| t.region());
    if handles.home == home {
        return;
    }
    let Some(mut materials) = materials else { return };
    let old = std::mem::replace(&mut handles.home, home);
    for (sim, children) in &chunks {
        if Some(sim.region) != old && Some(sim.region) != home {
            continue;
        }
        let material = handles.opaque_in(sim.region, &mut materials);
        for child in [children.body, children.top] {
            if let Ok(mut m) = drawn.get_mut(child) {
                m.0 = material.clone();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::commander::level::WorkingLevel;
    use crate::renderer::fortress_view::{build_uniform, CAP_DARKEN};
    use game::{RegionId, REGION_SIZE};

    fn home() -> RegionId {
        RegionId { x: 1, z: -2 }
    }

    /// The cut the stud material reads sits on a tile boundary of stud terrain:
    /// the top of the working level's 1.2 m tiles, through `game::tiles` alone.
    #[test]
    fn the_cut_sits_on_a_stud_tile_boundary() {
        let h = game::tile_height_units();
        assert_eq!(h, game::plate_units(game::TILE_PLATES as i32), "a level is one tile: six plates");
        for level in [-40, -1, 0, 7, 133] {
            let working = WorkingLevel { level: Some(level), cut: true };
            let u = build_uniform(&working, Some(home()));
            assert!(cutaway_active(&u));
            assert_eq!(u.cut.x, game::level_bottom_units(level + 1) + home().world_offset()[1]);
            assert_eq!(u.cut.y, CAP_DARKEN);
            // Whole plates: a tile boundary is a brick-grid plane.
            let plates = u.cut.x / game::plate_units(1);
            assert!((plates - plates.round()).abs() < 1e-3, "level {level}: {plates} plates");
            assert_eq!(plates.round() as i32 % game::TILE_PLATES as i32, 0);
        }
        // Level 10's cut is 11 tiles of 1.2 m up, not 11 m.
        let u = build_uniform(&WorkingLevel { level: Some(10), cut: true }, Some(home()));
        assert_eq!(u.cut.x - home().world_offset()[1], 11.0 * h);
    }

    #[test]
    fn params_carry_the_tile_height_and_the_shared_darkening_consts() {
        let p = fortress_params(true);
        assert_eq!(p.levels, Vec4::new(game::tile_height_units(), DEPTH_DARKEN_PER_LEVEL, DEPTH_DARKEN_FLOOR, 0.0));
        assert_eq!(p.flags.x, 1.0);
        assert_eq!(fortress_params(false).flags.x, 0.0, "glass has no cap");
        // The seam threshold separates seam lines from faces.
        assert!(STUD_EDGE_ALPHA < p.flags.y && p.flags.y < 1.0);
        assert!(STUD_EDGE_ALPHA > STUD_MASK_CUTOFF, "a seam line survives the mask");
    }

    /// The darkening curve, counted in tile levels.
    #[test]
    fn levels_below_the_working_level_darken_by_level() {
        let p = fortress_params(true);
        let h = game::tile_height_units();
        let cut = game::level_bottom_units(11);
        // The working level (the tile under the cut), top to bottom: full brightness.
        for y in [cut, cut - 0.5 * h, cut - h + 0.1] {
            assert_eq!(depth_darkening(&p, y, cut), 1.0, "y = {y}");
        }
        // Its floor (the top faces of the level below) is still the working level's.
        assert_eq!(depth_darkening(&p, cut - h, cut), 1.0);
        // One and two levels down.
        assert!((depth_darkening(&p, cut - 1.5 * h, cut) - (1.0 - DEPTH_DARKEN_PER_LEVEL)).abs() < 1e-6);
        assert!((depth_darkening(&p, cut - 2.5 * h, cut) - (1.0 - 2.0 * DEPTH_DARKEN_PER_LEVEL)).abs() < 1e-6);
        // Never under the floor.
        assert_eq!(depth_darkening(&p, cut - 500.0 * h, cut), DEPTH_DARKEN_FLOOR);
        // A depth in metres counts in 1.2 m levels: just over 4 m down is the third level below.
        let depth = 4.0 * game::BLOCK_UNITS as f32 + 1.0;
        assert!((depth_darkening(&p, cut - depth, cut) - (1.0 - 3.0 * DEPTH_DARKEN_PER_LEVEL)).abs() < 1e-6);
    }

    #[test]
    fn the_cut_removes_only_what_is_above_it_inside_the_home_region() {
        let working = WorkingLevel { level: Some(10), cut: true };
        let u = build_uniform(&working, Some(home()));
        let o = Vec3::from_array(home().world_offset());
        let cut = u.cut.x;
        let inside = o + Vec3::new(10.0, 0.0, 10.0);
        assert!(cut_away(&u, inside.with_y(cut + 1.0)));
        assert!(!cut_away(&u, inside.with_y(cut)), "a top face lying on the cut stays");
        assert!(!cut_away(&u, inside.with_y(cut - 1.0)));
        // Outside the home rect nothing is cut (half-open on the far edges).
        assert!(!cut_away(&u, (o + Vec3::new(-1.0, 0.0, 10.0)).with_y(cut + 50.0)));
        assert!(!cut_away(&u, (o + Vec3::new(REGION_SIZE, 0.0, 10.0)).with_y(cut + 50.0)));
        assert!(cut_away(&u, (o + Vec3::new(REGION_SIZE - 0.5, 0.0, 0.0)).with_y(cut + 50.0)));
        // Cut off: nothing.
        let off = build_uniform(&WorkingLevel { level: Some(10), cut: false }, Some(home()));
        assert!(!cut_away(&off, inside.with_y(cut + 50.0)));
    }

    #[test]
    fn the_opaque_material_is_masked_and_unculled_only_while_the_cut_is_on() {
        let on = build_uniform(&WorkingLevel { level: Some(3), cut: true }, Some(home()));
        let off = FortressViewUniform::default();
        let m = opaque_material(off);
        assert_eq!(m.base.alpha_mode, AlphaMode::Opaque);
        assert!(!StudFortressKey::from(&m.extension).uncull);
        let m = opaque_material(on);
        assert_eq!(m.base.alpha_mode, AlphaMode::Mask(STUD_MASK_CUTOFF));
        assert!(StudFortressKey::from(&m.extension).uncull, "back faces are the cap");
        let g = glass_material(on);
        assert_eq!(g.base.alpha_mode, AlphaMode::Blend);
        assert!(!StudFortressKey::from(&g.extension).uncull, "glass never caps");
    }

    /// The sync system brings both materials in step with the uniform and the
    /// home terrain, and flips the opaque one's alpha mode with the cut.
    #[test]
    fn materials_follow_the_fortress_view() {
        let mut app = App::new();
        app.init_resource::<Assets<StudTerrainMaterial>>()
            .init_resource::<StudMaterials>()
            .init_resource::<FortressViewUniform>()
            .insert_resource(WorkingLevel { level: Some(4), cut: true })
            .add_systems(Update, sync_stud_materials);
        let (opaque, glass, outside) = {
            let world = app.world_mut();
            world.resource_scope(|world, mut handles: Mut<StudMaterials>| {
                let mut assets = world.resource_mut::<Assets<StudTerrainMaterial>>();
                (handles.opaque(&mut assets), handles.glass(&mut assets), handles.outside(&mut assets))
            })
        };
        let on = build_uniform(app.world().resource::<WorkingLevel>(), Some(home()));
        *app.world_mut().resource_mut::<FortressViewUniform>() = on;
        let line = crate::renderer::boundaries::build_uniform(true, Some(game::RegionCoords::new(1, -2)));
        assert!(line.home_color.w > 0.0);
        app.insert_resource(line);
        app.update();
        let assets = app.world().resource::<Assets<StudTerrainMaterial>>();
        let (o, g) = (assets.get(&opaque).unwrap(), assets.get(&glass).unwrap());
        assert_eq!((o.extension.fortress, g.extension.fortress), (on, on));
        assert!(o.extension.boundaries == line && g.extension.boundaries == line, "the home boundary line too");
        assert_eq!(o.base.alpha_mode, AlphaMode::Mask(STUD_MASK_CUTOFF));
        assert_eq!(g.base.alpha_mode, AlphaMode::Blend);
        assert_eq!(o.extension.params, fortress_params(true));
        assert_eq!(g.extension.params, fortress_params(false));
        // The material of everything outside the home rect never takes the cut:
        // plain opaque, culled — but it does carry the boundary line.
        let out = assets.get(&outside).unwrap();
        assert_eq!(out.extension.fortress, FortressViewUniform::default());
        assert_eq!(out.base.alpha_mode, AlphaMode::Opaque);
        assert!(!StudFortressKey::from(&out.extension).uncull);
        assert!(out.extension.boundaries == line);
        assert_eq!(out.extension.params, fortress_params(false));
        // Cut off again: plain opaque.
        *app.world_mut().resource_mut::<FortressViewUniform>() = FortressViewUniform::default();
        app.update();
        let assets = app.world().resource::<Assets<StudTerrainMaterial>>();
        assert_eq!(assets.get(&opaque).unwrap().base.alpha_mode, AlphaMode::Opaque);
    }

    /// A chunk draws with the cut material only while its region is the home one.
    #[test]
    fn only_the_home_regions_chunks_take_the_cut_material() {
        use crate::renderer::bridge::SimEntity;
        use crate::renderer::commander::tiles::TileMirror;
        use crate::renderer::stud::systems::StudMeshChildren;
        let mut app = App::new();
        app.init_resource::<Assets<StudTerrainMaterial>>()
            .init_resource::<StudMaterials>()
            .init_resource::<TileMirror>()
            .add_systems(Update, retarget_stud_materials);
        let (cut, outside) = {
            let world = app.world_mut();
            world.resource_scope(|world, mut handles: Mut<StudMaterials>| {
                let mut assets = world.resource_mut::<Assets<StudTerrainMaterial>>();
                let other = RegionId { x: 5, z: 5 };
                // No home yet: every region's chunks are outside.
                assert_eq!(handles.opaque_in(home(), &mut assets), handles.outside(&mut assets));
                assert_eq!(handles.opaque_in(other, &mut assets), handles.outside(&mut assets));
                (handles.opaque(&mut assets), handles.outside(&mut assets))
            })
        };
        assert_ne!(cut, outside);
        let key = |n: u64| game::EntityKey::from(slotmapd::KeyData::from_ffi((1 << 32) | n));
        let mut chunk = |region: RegionId, n: u64| {
            let w = app.world_mut();
            let body = w.spawn(MeshMaterial3d(outside.clone())).id();
            let top = w.spawn(MeshMaterial3d(outside.clone())).id();
            let glass = w.spawn_empty().id();
            w.spawn((SimEntity { region, key: key(n) }, StudMeshChildren { body, top, glass }));
            (body, top)
        };
        let east = RegionId { x: home().x + 1, z: home().z };
        let (in_home, in_east) = (chunk(home(), 1), chunk(east, 2));
        let material = |app: &App, e: Entity| app.world().get::<MeshMaterial3d<StudTerrainMaterial>>(e).unwrap().0.clone();
        app.update();
        assert_eq!(material(&app, in_home.0), outside, "no home: nothing changes");
        // The home region is set: its chunks (body and detail) take the cut material.
        app.world_mut().resource_mut::<TileMirror>().set_home(Some(home()));
        app.update();
        assert_eq!((material(&app, in_home.0), material(&app, in_home.1)), (cut.clone(), cut.clone()));
        assert_eq!((material(&app, in_east.0), material(&app, in_east.1)), (outside.clone(), outside.clone()));
        // The home moves east: they swap.
        app.world_mut().resource_mut::<TileMirror>().set_home(Some(east));
        app.update();
        assert_eq!(material(&app, in_home.0), outside);
        assert_eq!(material(&app, in_east.0), cut);
        let world = app.world_mut();
        world.resource_scope(|world, mut handles: Mut<StudMaterials>| {
            let mut assets = world.resource_mut::<Assets<StudTerrainMaterial>>();
            assert_eq!(handles.opaque_in(east, &mut assets), cut, "a chunk meshed now gets the same");
            assert_eq!(handles.opaque_in(home(), &mut assets), outside);
        });
    }

    const PBR_STUBS: [(&str, &str); 5] = [
        (
            "forward_io.wgsl",
            r#"
#define_import_path bevy_pbr::forward_io
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
#ifdef VERTEX_COLORS
    @location(5) color: vec4<f32>,
#endif
}
struct FragmentOutput {
    @location(0) color: vec4<f32>,
}
"#,
        ),
        (
            "pbr_types.wgsl",
            r#"
#define_import_path bevy_pbr::pbr_types
struct StandardMaterial { base_color: vec4<f32>, flags: u32 }
struct PbrInput { material: StandardMaterial, world_position: vec4<f32>, world_normal: vec3<f32> }
"#,
        ),
        (
            "pbr_fragment.wgsl",
            r#"
#define_import_path bevy_pbr::pbr_fragment
#import bevy_pbr::{pbr_types, forward_io::VertexOutput}
fn pbr_input_from_standard_material(in: VertexOutput, is_front: bool) -> pbr_types::PbrInput {
    var out: pbr_types::PbrInput;
    out.world_position = in.world_position;
    out.world_normal = in.world_normal;
    out.material.base_color = vec4<f32>(1.0);
#ifdef VERTEX_COLORS
    out.material.base_color = in.color;
#endif
    return out;
}
"#,
        ),
        (
            "pbr_functions.wgsl",
            r#"
#define_import_path bevy_pbr::pbr_functions
#import bevy_pbr::pbr_types
fn alpha_discard(material: pbr_types::StandardMaterial, output_color: vec4<f32>) -> vec4<f32> { return output_color; }
fn apply_pbr_lighting(in: pbr_types::PbrInput) -> vec4<f32> { return in.material.base_color; }
fn main_pass_post_lighting_processing(pbr_input: pbr_types::PbrInput, input_color: vec4<f32>) -> vec4<f32> { return input_color; }
"#,
        ),
        (
            "prepass_io.wgsl",
            r#"
#define_import_path bevy_pbr::prepass_io
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
#ifdef NORMAL_PREPASS_OR_DEFERRED_PREPASS
    @location(2) world_normal: vec3<f32>,
#endif
    @location(4) world_position: vec4<f32>,
#ifdef MOTION_VECTOR_PREPASS
    @location(5) previous_world_position: vec4<f32>,
#endif
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    @location(6) unclipped_depth: f32,
#endif
}
#ifdef PREPASS_FRAGMENT
struct FragmentOutput {
#ifdef NORMAL_PREPASS
    @location(0) normal: vec4<f32>,
#endif
#ifdef MOTION_VECTOR_PREPASS
    @location(1) motion_vector: vec2<f32>,
#endif
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    @builtin(frag_depth) frag_depth: f32,
#endif
}
#endif
"#,
        ),
    ];

    const PREPASS_FUNCTIONS_STUB: (&str, &str) = (
        "pbr_prepass_functions.wgsl",
        r#"
#define_import_path bevy_pbr::pbr_prepass_functions
fn calculate_motion_vector(world_position: vec4<f32>, previous_world_position: vec4<f32>) -> vec2<f32> {
    return world_position.xy - previous_world_position.xy;
}
"#,
    );

    /// `stud_terrain.wgsl` composes and validates in every variant the
    /// material can be specialized to: forward (with and without vertex
    /// colours), and the prepass with no fragment output, with the shadow
    /// pass's depth emulation, and with the normal / motion-vector targets.
    /// (Against stubs of Bevy's PBR modules, like every shader test here: an
    /// upstream signature change only shows on a real adapter.)
    #[test]
    fn stud_terrain_shader_is_valid_wgsl_in_every_variant() {
        use crate::renderer::shader_test::{validate_shader, IMPORTS};
        use bevy::shader::ShaderDefVal;
        let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/shaders/stud_terrain.wgsl"))
            .expect("stud_terrain.wgsl");
        let imports: Vec<(&str, &str)> = IMPORTS.into_iter().chain(PBR_STUBS).chain([PREPASS_FUNCTIONS_STUB]).collect();
        let variants: [&[&str]; 7] = [
            &[],
            &["VERTEX_COLORS"],
            &["PREPASS_PIPELINE"],
            &["PREPASS_PIPELINE", "MAY_DISCARD"],
            &["PREPASS_PIPELINE", "PREPASS_FRAGMENT", "UNCLIPPED_DEPTH_ORTHO_EMULATION"],
            &["PREPASS_PIPELINE", "PREPASS_FRAGMENT", "NORMAL_PREPASS", "NORMAL_PREPASS_OR_DEFERRED_PREPASS"],
            &[
                "PREPASS_PIPELINE",
                "PREPASS_FRAGMENT",
                "NORMAL_PREPASS",
                "NORMAL_PREPASS_OR_DEFERRED_PREPASS",
                "MOTION_VECTOR_PREPASS",
                "UNCLIPPED_DEPTH_ORTHO_EMULATION",
            ],
        ];
        for names in variants {
            let mut defs: Vec<ShaderDefVal> = names.iter().map(|&n| n.into()).collect();
            defs.push(ShaderDefVal::UInt("MATERIAL_BIND_GROUP".into(), 3));
            validate_shader(source.clone(), &defs, &imports).unwrap_or_else(|e| panic!("stud_terrain.wgsl with {names:?}: {e:?}"));
        }
        // The shader restates none of the shared numbers: they come in `params`.
        for name in ["DEPTH_DARKEN_PER_LEVEL", "DEPTH_DARKEN_FLOOR", "BLOCK_UNITS"] {
            assert!(!source.contains(&format!("const {name}")), "stud_terrain.wgsl must not restate {name}");
        }
        assert!(source.contains("@binding(100) var<uniform> fortress: FortressView;"));
        assert!(source.contains("@binding(101) var<uniform> params: StudFortressParams;"));
        assert!(source.contains("@binding(102) var<uniform> boundaries: TerrainBoundaries;"));
        // The boundary line's shape (`boundaries.rs` is the source of the fields).
        for line in [
            "let q = max(b.home_rect.xy - p, p - b.home_rect.zw);",
            "let sd = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0);",
            "let w = max(b.shape.x * unit_px, b.shape.y);",
            "let line = smoothstep(0.0, 1.0, sd_px) * (1.0 - smoothstep(w, w + 1.0, sd_px));",
        ] {
            assert!(source.contains(line), "boundary line drifted: `{line}`");
        }
    }
}
