//! Fortress view uniform: the z-level cutaway and the designation overlay
//! for the pulled-quad terrain path. One small uniform at group(1) binding 6
//! of `voxel_pull.wgsl` / `voxel_pull_depth.wgsl`, mirrored there as
//! `FortressView`.
//!
//! While the cut is on (`cutaway_active`), the pull pipelines specialize with
//! the `CUTAWAY` shader def and no face culling: quads wholly above the cut
//! plane collapse in the vertex stage, fragments above it are discarded (in
//! the forward, depth-prepass and shadow passes alike), and a back face that
//! shows through is drawn as the cut's cap — shaded where the eye ray crosses
//! the plane, with its block's top texture, facing up, darkened by
//! [`CAP_DARKEN`]. Front faces below the cut darken per level down
//! ([`DEPTH_DARKEN_PER_LEVEL`], never below [`DEPTH_DARKEN_FLOOR`]). The GPU
//! face-direction cull keeps every face range meanwhile: the cap is made of
//! back faces. All of it applies inside the home region's xz rect only
//! (`origin.xz` .. `origin.xz + cut.w`, decided per quad): outside it — the
//! neighbouring terrain and the LOD rings — the terrain draws exactly as
//! without the cut, the shader dropping back faces itself there.
//!
//! The designation overlay ([`OverlayGrid`]) is one byte per block cell of
//! the home region on the working level and the [`OVERLAY_LEVELS`]` - 1`
//! below it ([`overlay_byte`]: the designation kind, plus the drag preview's
//! bits), uploaded to an R8Uint 3D texture ([`OverlayTexture`]) read by the
//! forward shader at group(1) binding 7 — tinting the faces of designated
//! blocks, and the cut's cap — and by the slice plane (`slice_plane.rs`) for
//! implied rock at the cut, which has no face. It is on (`cut.z`) whenever
//! there is a home region and a working level, cut on or off. Only the
//! changed sub-box is re-uploaded; a level change re-uploads it all.
//!
//! Not supported by the per-chunk Mesh fallback (WebGL2) or the brick far
//! renderer: there the terrain draws uncut and untinted.

use std::collections::BTreeMap;

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::{
    extract_resource::{ExtractResource, ExtractResourcePlugin},
    render_asset::RenderAssets,
    render_resource::{
        Extent3d, Origin3d, ShaderType, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect,
        TextureDimension, TextureFormat, TextureUsages,
    },
    renderer::RenderQueue,
    texture::GpuImage,
    Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
};
use game::{Designation, DesignationKind, Invalid, RegionId, TileBox, TilePos, REGION_BLOCKS, REGION_SIZE};

use super::commander::level::{self, WorkingLevel};
use super::commander::tiles::TileMirror;

/// How much darker (0..1) the cap is than a lit top face.
pub const CAP_DARKEN: f32 = 0.25;
/// Brightness lost per level below the cut (front faces). Read only by the
/// shader, as a WGSL `const` mirroring this one (test-pinned).
#[cfg_attr(not(test), allow(dead_code))]
pub const DEPTH_DARKEN_PER_LEVEL: f32 = 0.12;
/// The darkest a level below the cut gets. WGSL-mirrored like the above.
#[cfg_attr(not(test), allow(dead_code))]
pub const DEPTH_DARKEN_FLOOR: f32 = 0.35;

/// Overlay texture layers: the working level and the 15 below it.
pub const OVERLAY_LEVELS: u32 = 16;
/// Overlay byte: the designation kind in bits 0..=2 (Build and none are 0 —
/// a Build sits in air, so it has no face to tint). WGSL-mirrored, like the
/// bits below.
#[cfg_attr(not(test), allow(dead_code))]
pub const OVERLAY_KIND_MASK: u8 = 0b111;
pub const OVERLAY_DIG: u8 = 1;
pub const OVERLAY_DECONSTRUCT: u8 = 2;
pub const OVERLAY_CHOP: u8 = 3;
/// The cell is in the drag preview.
pub const OVERLAY_PREVIEW: u8 = 1 << 4;
/// ... and the preview's order would be invalid there.
pub const OVERLAY_INVALID: u8 = 1 << 5;

/// A cell's overlay byte from its designation and its drag-preview result.
pub fn overlay_byte(d: Option<&Designation>, preview: Option<Result<(), Invalid>>) -> u8 {
    let kind = match d.map(|d| d.kind) {
        Some(DesignationKind::Dig) => OVERLAY_DIG,
        Some(DesignationKind::Deconstruct) => OVERLAY_DECONSTRUCT,
        Some(DesignationKind::Chop) => OVERLAY_CHOP,
        Some(DesignationKind::Build(_)) | None => 0,
    };
    let preview = match preview {
        None => 0,
        Some(Ok(())) => OVERLAY_PREVIEW,
        Some(Err(_)) => OVERLAY_PREVIEW | OVERLAY_INVALID,
    };
    kind | preview
}

/// The designation tool's drag in progress: the tiles it would cover and
/// whether the order is valid on each. Empty when no drag is on. Filled by
/// the designation tools (`commander::designate`); the overlay reads `cells`
/// and `kind` (`None` for an erase).
#[derive(Resource, Default)]
pub struct DragPreview {
    pub area: Option<TileBox>,
    pub kind: Option<DesignationKind>,
    pub cells: Vec<(TilePos, Result<(), Invalid>)>,
}

/// The overlay's base level (texture layer 0) for working level `level`:
/// the working level is the top layer.
pub fn overlay_base_level(level: i32) -> i32 {
    level - (OVERLAY_LEVELS as i32 - 1)
}

/// The overlay texture's extent: `W × D × OVERLAY_LEVELS` over the region.
pub fn overlay_dims() -> UVec3 {
    UVec3::new(REGION_BLOCKS as u32, REGION_BLOCKS as u32, OVERLAY_LEVELS)
}

/// The CPU side of the overlay texture: one [`overlay_byte`] per cell of the
/// home region on levels `base_level ..  base_level + OVERLAY_LEVELS`.
/// Texture coordinates are `(x, z, y - base_level)` with x and z
/// region-local blocks, byte index `x + z * W + layer * W * D`.
#[derive(Resource)]
pub struct OverlayGrid {
    pub base_level: i32,
    /// Texture extent: (W = region x blocks, D = region z blocks, layers).
    pub dims: UVec3,
    pub bytes: Vec<u8>,
    /// Texture-space box changed by this frame's rebuild, `[min, max)`;
    /// cleared at the start of the next frame's update (after extraction).
    pub dirty: Option<(UVec3, UVec3)>,
}

impl Default for OverlayGrid {
    fn default() -> Self {
        let dims = overlay_dims();
        Self { base_level: 0, dims, bytes: vec![0; dims.element_product() as usize], dirty: None }
    }
}

impl OverlayGrid {
    /// Byte index of tile `p` with layer 0 at `base`, `None` outside the grid.
    fn index_at(dims: UVec3, base: i32, p: TilePos) -> Option<usize> {
        let at = [p[0], p[2], p[1] - base];
        let inside = at.iter().zip(dims.to_array()).all(|(&c, d)| c >= 0 && (c as u32) < d);
        inside.then(|| (at[0] as u32 + at[1] as u32 * dims.x + at[2] as u32 * dims.x * dims.y) as usize)
    }

    /// Byte index of tile `p`, `None` outside the grid.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn index(&self, p: TilePos) -> Option<usize> {
        Self::index_at(self.dims, self.base_level, p)
    }

    /// Rebuild every byte for `base_level` from the designations and the
    /// drag preview (whose cells show the drag's kind for a dig or
    /// deconstruct drag, else keep their designation's kind), marking the
    /// changed box dirty — all of it when `full`.
    pub fn rebuild(
        &mut self,
        base_level: i32,
        designations: &BTreeMap<TilePos, Designation>,
        preview: &DragPreview,
        full: bool,
    ) {
        let mut next = vec![0u8; self.bytes.len()];
        for (p, d) in designations {
            if let Some(i) = Self::index_at(self.dims, base_level, *p) {
                next[i] = overlay_byte(Some(d), None);
            }
        }
        // A dig, deconstruct or chop drag previews its own kind; an erase (or
        // a build, which has no face tint) keeps each tile's designation.
        let drag_kind = preview
            .kind
            .filter(|k| matches!(k, DesignationKind::Dig | DesignationKind::Deconstruct | DesignationKind::Chop))
            .map(|kind| Designation { kind, op: game::OpId::default(), priority: game::DEFAULT_PRIORITY });
        for (p, result) in &preview.cells {
            if let Some(i) = Self::index_at(self.dims, base_level, *p) {
                next[i] = overlay_byte(drag_kind.as_ref().or(designations.get(p)), Some(*result));
            }
        }
        let dirty = if full { Some((UVec3::ZERO, self.dims)) } else { diff_box(&self.bytes, &next, self.dims) };
        self.base_level = base_level;
        self.bytes = next;
        self.dirty = union_box(self.dirty, dirty);
    }
}

/// The texture-space box `[min, max)` holding every byte that differs.
fn diff_box(old: &[u8], new: &[u8], dims: UVec3) -> Option<(UVec3, UVec3)> {
    let w = dims.x as usize;
    let mut found: Option<(UVec3, UVec3)> = None;
    for (row, (a, b)) in old.chunks_exact(w).zip(new.chunks_exact(w)).enumerate() {
        if a == b {
            continue;
        }
        let first = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(0) as u32;
        let last = a.iter().zip(b).rposition(|(x, y)| x != y).unwrap_or(0) as u32;
        let (z, layer) = (row as u32 % dims.y, row as u32 / dims.y);
        found = union_box(found, Some((UVec3::new(first, z, layer), UVec3::new(last + 1, z + 1, layer + 1))));
    }
    found
}

fn union_box(a: Option<(UVec3, UVec3)>, b: Option<(UVec3, UVec3)>) -> Option<(UVec3, UVec3)> {
    match (a, b) {
        (Some((a0, a1)), Some((b0, b1))) => Some((a0.min(b0), a1.max(b1))),
        (a, b) => a.or(b),
    }
}

/// Copy the texture-space box `[lo, hi)` of `src` into `dst` (same dims).
fn copy_box(src: &[u8], dst: &mut [u8], dims: UVec3, (lo, hi): (UVec3, UVec3)) {
    for layer in lo.z..hi.z {
        for z in lo.y..hi.y {
            let row = ((z + layer * dims.y) * dims.x) as usize;
            let range = row + lo.x as usize..row + hi.x as usize;
            dst[range.clone()].copy_from_slice(&src[range]);
        }
    }
}

/// What the overlay grid was last built for.
#[derive(Default)]
pub struct OverlayBuilt {
    region: Option<RegionId>,
    base_level: Option<i32>,
    version: u64,
}

/// Rebuild the overlay grid when the mirror's version, the working level,
/// the home region or the drag preview changes. A level or region change
/// marks the whole texture dirty.
pub fn update_overlay_grid(
    working: Res<WorkingLevel>,
    tiles: Res<TileMirror>,
    preview: Res<DragPreview>,
    mut grid: ResMut<OverlayGrid>,
    mut built: Local<OverlayBuilt>,
) {
    // Last frame's box has been extracted by now.
    if grid.dirty.is_some() {
        grid.dirty = None;
    }
    let (Some(region), Some(level)) = (tiles.region(), working.level) else { return };
    let base = overlay_base_level(level);
    let full = built.region != Some(region) || built.base_level != Some(base);
    if !full && built.version == tiles.version() && !preview.is_changed() {
        return;
    }
    grid.rebuild(base, tiles.designations(), &preview, full);
    *built = OverlayBuilt { region: Some(region), base_level: Some(base), version: tiles.version() };
}

/// The overlay texture: an R8Uint `W × D × OVERLAY_LEVELS` 3D image, render
/// world only. Its GPU texture is written in place by [`upload_overlay`]
/// (never through the asset, which would re-upload it whole); the pull
/// path's bind group and the slice plane's material both bind it.
#[derive(Resource, Clone, ExtractResource)]
pub struct OverlayTexture(pub Handle<Image>);

impl FromWorld for OverlayTexture {
    fn from_world(world: &mut World) -> Self {
        let dims = overlay_dims();
        let mut image = Image::new(
            Extent3d { width: dims.x, height: dims.y, depth_or_array_layers: dims.z },
            TextureDimension::D3,
            vec![0; dims.element_product() as usize],
            TextureFormat::R8Uint,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;
        Self(world.resource_mut::<Assets<Image>>().add(image))
    }
}

/// Render-world copy of the overlay grid and the box still to upload.
#[derive(Resource, Default)]
pub struct OverlayUpload {
    bytes: Vec<u8>,
    dims: UVec3,
    pending: Option<(UVec3, UVec3)>,
}

fn extract_overlay(grid: Extract<Option<Res<OverlayGrid>>>, mut upload: ResMut<OverlayUpload>) {
    let Some(grid) = grid.as_deref() else { return };
    let Some(dirty) = grid.dirty else { return };
    if upload.bytes.len() != grid.bytes.len() {
        upload.bytes = grid.bytes.clone();
        upload.dims = grid.dims;
        upload.pending = Some((UVec3::ZERO, grid.dims));
        return;
    }
    copy_box(&grid.bytes, &mut upload.bytes, grid.dims, dirty);
    upload.pending = union_box(upload.pending, Some(dirty));
}

/// Write the pending box into the overlay texture, once its GPU image exists
/// (until then the boxes accumulate).
fn upload_overlay(
    mut upload: ResMut<OverlayUpload>,
    texture: Option<Res<OverlayTexture>>,
    images: Res<RenderAssets<GpuImage>>,
    queue: Res<RenderQueue>,
) {
    let Some((lo, hi)) = upload.pending else { return };
    let Some(gpu) = texture.and_then(|t| images.get(&t.0)) else { return };
    let dims = upload.dims;
    queue.write_texture(
        TexelCopyTextureInfo {
            texture: &gpu.texture,
            mip_level: 0,
            origin: Origin3d { x: lo.x, y: lo.y, z: lo.z },
            aspect: TextureAspect::All,
        },
        &upload.bytes,
        TexelCopyBufferLayout {
            offset: u64::from(lo.x + lo.y * dims.x + lo.z * dims.x * dims.y),
            bytes_per_row: Some(dims.x),
            rows_per_image: Some(dims.y),
        },
        Extent3d { width: hi.x - lo.x, height: hi.y - lo.y, depth_or_array_layers: hi.z - lo.z },
    );
    upload.pending = None;
}

/// The overlay's resources, texture and render-world upload. Its grid
/// rebuild runs in the commander plugin (`update_overlay_grid`).
pub struct FortressOverlayPlugin;

impl Plugin for FortressOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OverlayGrid>()
            .init_resource::<DragPreview>()
            .init_resource::<OverlayTexture>()
            .add_plugins(ExtractResourcePlugin::<OverlayTexture>::default());
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_resource::<OverlayUpload>()
            .add_systems(ExtractSchedule, extract_overlay)
            .add_systems(Render, upload_overlay.in_set(RenderSystems::PrepareResources));
    }
}

/// Layout mirrored by `FortressView` in `voxel_pull.wgsl` and
/// `voxel_pull_depth.wgsl`. Extracted into the render world, where
/// `prepare_chunk_quads` uploads it and the queue systems pick the `CUTAWAY`
/// pipeline variant from it.
#[derive(Resource, Clone, Copy, PartialEq, Debug, ExtractResource, ShaderType)]
pub struct FortressViewUniform {
    /// x: cut plane world y (f32::MAX = off); y: cap darkening 0..1; z: overlay on (0/1);
    /// w: home region extent (world units) — the cut applies in [origin.xz, origin.xz + w)
    pub cut: Vec4,
    /// xyz: home region world offset; w: overlay base level (block y of texture layer 0) as f32
    pub origin: Vec4,
    /// x: overlay width (blocks, region x), y: depth (region z), z: layers; w: unused
    pub overlay_dims: UVec4,
}

impl Default for FortressViewUniform {
    fn default() -> Self {
        Self {
            cut: Vec4::new(f32::MAX, 0.0, 0.0, 0.0),
            origin: Vec4::ZERO,
            overlay_dims: UVec4::ZERO,
        }
    }
}

/// Whether the cut is on (the pipelines then take the `CUTAWAY` variant).
pub fn cutaway_active(u: &FortressViewUniform) -> bool {
    u.cut.x < f32::MAX
}

/// Pure uniform builder: the cut follows the working level in the home
/// region, off when the cut is toggled off, the level is unknown or there is
/// no home. The overlay is on whenever the home and the level are known,
/// cut on or off, its layers ending at the working level.
pub fn build_uniform(working: &WorkingLevel, home: Option<RegionId>) -> FortressViewUniform {
    let mut u = FortressViewUniform::default();
    let Some(region) = home else { return u };
    u.origin = Vec3::from_array(region.world_offset()).extend(0.0);
    u.cut.w = REGION_SIZE;
    if let Some(level) = working.level {
        u.cut.z = 1.0;
        u.origin.w = overlay_base_level(level) as f32;
        u.overlay_dims = overlay_dims().extend(0);
    }
    if let Some(cut) = level::cut_y(working, home) {
        u.cut.x = cut;
        u.cut.y = CAP_DARKEN;
    }
    u
}

/// Rebuild the uniform from the working level. Only writes on a change, so
/// extraction re-sends it only then.
pub fn update_fortress_view(
    working: Res<WorkingLevel>,
    tiles: Res<TileMirror>,
    mut uniform: ResMut<FortressViewUniform>,
) {
    let next = build_uniform(&working, tiles.region());
    if uniform.as_ref() != &next {
        *uniform = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_is_off_by_default() {
        assert!(!cutaway_active(&Default::default()));
    }

    #[test]
    fn uniform_tracks_working_level() {
        let home = Some(RegionId { x: 0, z: 0 });
        let working = WorkingLevel { level: Some(10), cut: true };
        let u = build_uniform(&working, home);
        assert!(cutaway_active(&u));
        assert_eq!(u.cut.x, 176.0);
        assert_eq!(u.cut.y, CAP_DARKEN);
        assert_eq!(u.cut.z, 1.0, "the overlay is on with a home and a level");
        assert_eq!(u.origin.w, (10 - 15) as f32, "the working level is the top layer");
        assert_eq!(u.overlay_dims, UVec4::new(REGION_BLOCKS as u32, REGION_BLOCKS as u32, OVERLAY_LEVELS, 0));

        let off = WorkingLevel { level: Some(10), cut: false };
        assert!(!cutaway_active(&build_uniform(&off, home)));
        assert_eq!(build_uniform(&off, home).cut.z, 1.0, "the overlay stays on with the cut off");
        assert!(!cutaway_active(&build_uniform(&working, None)));
        assert_eq!(build_uniform(&working, None).cut.z, 0.0);
        assert_eq!(build_uniform(&WorkingLevel { level: None, cut: true }, home).cut.z, 0.0);
    }

    #[test]
    fn overlay_grid_covers_the_region() {
        assert_eq!(overlay_dims(), UVec3::new(REGION_BLOCKS as u32, REGION_BLOCKS as u32, OVERLAY_LEVELS));
        assert_eq!(OverlayGrid::default().dims, overlay_dims());
        assert_eq!(OverlayGrid::default().bytes.len(), overlay_dims().element_product() as usize);
    }

    fn designation(kind: DesignationKind) -> Designation {
        Designation { kind, op: game::OpId::default(), priority: 4 }
    }

    #[test]
    fn overlay_byte_encodes_kind_and_preview_bits() {
        let dig = designation(DesignationKind::Dig);
        let deconstruct = designation(DesignationKind::Deconstruct);
        let build = designation(DesignationKind::Build(game::BlockId(2)));
        assert_eq!(overlay_byte(Some(&dig), None), 1);
        assert_eq!(overlay_byte(Some(&deconstruct), Some(Ok(()))), 2 | 16);
        assert_eq!(overlay_byte(None, Some(Err(Invalid::Bedrock))), 16 | 32);
        assert_eq!(overlay_byte(Some(&build), None), 0);
        assert_eq!(overlay_byte(Some(&designation(DesignationKind::Chop)), None), 3);
        assert_eq!(overlay_byte(None, None), 0);
    }

    #[test]
    fn grid_index_is_x_z_layer() {
        let grid = OverlayGrid { base_level: 20, ..default() };
        let (w, d) = (grid.dims.x as usize, grid.dims.y as usize);
        assert_eq!(grid.index([3, 22, 5]), Some(3 + 5 * w + 2 * w * d));
        assert_eq!(grid.index([0, 20, 0]), Some(0));
        assert_eq!(grid.index([3, 19, 5]), None, "below the bottom layer");
        assert_eq!(grid.index([3, 20 + OVERLAY_LEVELS as i32, 5]), None, "above the working level");
        assert_eq!(grid.index([-1, 22, 5]), None);
        assert_eq!(grid.index([REGION_BLOCKS as i32, 22, 5]), None);
    }

    #[test]
    fn grid_marks_dirty_box_for_a_single_change() {
        let mut grid = OverlayGrid::default();
        let none = DragPreview::default();
        let mut marks = BTreeMap::new();
        grid.rebuild(20, &marks, &none, true);
        assert_eq!(grid.dirty, Some((UVec3::ZERO, grid.dims)), "a full rebuild marks everything");

        grid.dirty = None;
        marks.insert([3, 22, 5], designation(DesignationKind::Dig));
        grid.rebuild(20, &marks, &none, false);
        assert_eq!(grid.dirty, Some((UVec3::new(3, 5, 2), UVec3::new(4, 6, 3))));
        assert_eq!(grid.bytes[grid.index([3, 22, 5]).unwrap()], OVERLAY_DIG);

        grid.dirty = None;
        grid.rebuild(20, &marks, &none, false);
        assert_eq!(grid.dirty, None, "nothing changed");

        // A preview cell over it keeps the kind and adds the preview bits.
        let preview = DragPreview { cells: vec![([3, 22, 5], Err(Invalid::Bedrock)), ([9, 22, 1], Ok(()))], ..default() };
        grid.rebuild(20, &marks, &preview, false);
        assert_eq!(grid.bytes[grid.index([3, 22, 5]).unwrap()], OVERLAY_DIG | OVERLAY_PREVIEW | OVERLAY_INVALID);
        assert_eq!(grid.bytes[grid.index([9, 22, 1]).unwrap()], OVERLAY_PREVIEW);
        assert_eq!(grid.dirty, Some((UVec3::new(3, 1, 2), UVec3::new(10, 6, 3))));
    }

    /// A dig / deconstruct drag previews its own kind; an erase keeps the
    /// tile's.
    #[test]
    fn preview_shows_the_drags_kind() {
        let mut grid = OverlayGrid::default();
        let mut marks = BTreeMap::new();
        marks.insert([3, 22, 5], designation(DesignationKind::Dig));
        let cells = vec![([3, 22, 5], Ok(())), ([9, 22, 1], Err(Invalid::NothingToDig))];
        let dig = DragPreview { kind: Some(DesignationKind::Dig), cells: cells.clone(), ..default() };
        grid.rebuild(20, &BTreeMap::new(), &dig, true);
        assert_eq!(grid.bytes[grid.index([3, 22, 5]).unwrap()], OVERLAY_DIG | OVERLAY_PREVIEW);
        assert_eq!(grid.bytes[grid.index([9, 22, 1]).unwrap()], OVERLAY_DIG | OVERLAY_PREVIEW | OVERLAY_INVALID);

        let deconstruct = DragPreview { kind: Some(DesignationKind::Deconstruct), cells: cells.clone(), ..default() };
        grid.rebuild(20, &marks, &deconstruct, false);
        assert_eq!(grid.bytes[grid.index([3, 22, 5]).unwrap()], OVERLAY_DECONSTRUCT | OVERLAY_PREVIEW);

        let erase = DragPreview { kind: None, cells, ..default() };
        grid.rebuild(20, &marks, &erase, false);
        assert_eq!(grid.bytes[grid.index([3, 22, 5]).unwrap()], OVERLAY_DIG | OVERLAY_PREVIEW);
        assert_eq!(grid.bytes[grid.index([9, 22, 1]).unwrap()], OVERLAY_PREVIEW | OVERLAY_INVALID);
    }

    #[test]
    fn copy_box_moves_only_the_box() {
        let dims = UVec3::new(4, 3, 2);
        let src: Vec<u8> = (0..24).collect();
        let mut dst = vec![0u8; 24];
        copy_box(&src, &mut dst, dims, (UVec3::new(1, 1, 1), UVec3::new(3, 2, 2)));
        let copied: Vec<usize> = dst.iter().enumerate().filter(|(_, &b)| b != 0).map(|(i, _)| i).collect();
        assert_eq!(copied, vec![17, 18]);
        assert_eq!(&dst[17..19], &src[17..19]);
    }

    #[test]
    fn shader_mirrors_the_overlay_bits() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/shaders/voxel_pull.wgsl"
        ))
        .expect("voxel_pull.wgsl");
        for (name, value) in [
            ("OVERLAY_KIND_MASK", OVERLAY_KIND_MASK),
            ("OVERLAY_DIG", OVERLAY_DIG),
            ("OVERLAY_DECONSTRUCT", OVERLAY_DECONSTRUCT),
            ("OVERLAY_CHOP", OVERLAY_CHOP),
            ("OVERLAY_PREVIEW", OVERLAY_PREVIEW),
            ("OVERLAY_INVALID", OVERLAY_INVALID),
        ] {
            let line = format!("const {name}: u32 = {value}u;");
            assert!(source.contains(&line), "voxel_pull.wgsl must declare `{line}`");
        }
    }

    /// The overlay patterns are one WGSL block copied into both shaders that
    /// draw them (WGSL has no shared import here): the copies must match.
    #[test]
    fn overlay_pattern_block_is_identical_in_both_shaders() {
        let block = |file: &str| {
            let source = std::fs::read_to_string(format!(
                "{}/../../assets/shaders/{file}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap_or_else(|e| panic!("{file}: {e}"));
            let begin = source.find(OVERLAY_BLOCK_BEGIN).unwrap_or_else(|| panic!("{file}: no overlay block"));
            let end = source.find(OVERLAY_BLOCK_END).unwrap_or_else(|| panic!("{file}: no overlay block end"));
            source[begin..end].to_string()
        };
        assert_eq!(block("voxel_pull.wgsl"), block("slice_plane.wgsl"));
    }

    const OVERLAY_BLOCK_BEGIN: &str = "// BEGIN fortress overlay patterns";
    const OVERLAY_BLOCK_END: &str = "// END fortress overlay patterns";

    #[test]
    fn shader_mirrors_the_depth_constants() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/shaders/voxel_pull.wgsl"
        ))
        .expect("voxel_pull.wgsl");
        for (name, value) in [
            ("DEPTH_DARKEN_PER_LEVEL", DEPTH_DARKEN_PER_LEVEL),
            ("DEPTH_DARKEN_FLOOR", DEPTH_DARKEN_FLOOR),
        ] {
            let line = format!("const {name}: f32 = {value};");
            assert!(source.contains(&line), "voxel_pull.wgsl must declare `{line}`");
        }
    }

    /// The cut is confined to the home region: its rect is origin.xz ..
    /// origin.xz + cut.w.
    #[test]
    fn uniform_carries_the_home_region_rect() {
        let working = WorkingLevel { level: Some(10), cut: true };
        let u = build_uniform(&working, Some(RegionId { x: 2, z: -1 }));
        assert_eq!(u.origin.truncate(), Vec3::new(2.0, 0.0, -1.0) * REGION_SIZE);
        assert_eq!(u.cut.w, REGION_SIZE);
    }
}
