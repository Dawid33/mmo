//! Designations shown on stud terrain. The voxel overlay tints block faces in
//! the pulled-quad shader (`fortress_view.rs`), which stud meshes do not go
//! through, so a stud home region gets one plain mesh instead: a translucent
//! box per run of designated tiles, coloured by kind, rebuilt when the tile
//! mirror changes. Cheap and correct rather than pretty (stud world plan P7a);
//! the shader overlay on stud materials replaces it later.
//!
//! **The cut.** The boxes are not drawn with the terrain's fortress material:
//! a box stands [`OVERLAY_INFLATE`] out of its tiles, so the top of a box on the
//! working level pokes through the cut plane and a per-fragment cut would shave
//! it. Instead the mesh is built without the tiles the cut hides
//! ([`designation_boxes`]' `top_level`): while the cut is on, designations above
//! the working level are left out and those on it are drawn whole. Unlit, so
//! the levels below are not darkened — a designation reads the same at any depth.

use std::collections::BTreeMap;

use bevy::asset::RenderAssetUsages;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use game::{Designation, DesignationKind, RegionId, TerrainKind, TileBox, TilePos};

use crate::renderer::commander::designate::merge_runs;
use crate::renderer::commander::level::WorkingLevel;
use crate::renderer::commander::tiles::TileMirror;
use crate::session::SessionScoped;

/// Most boxes drawn (runs of designated tiles); the rest are left out.
pub const MAX_OVERLAY_BOXES: usize = 8192;
/// How far a box stands out of its tiles, sim units, so it does not z-fight
/// the bricks it covers.
pub const OVERLAY_INFLATE: f32 = 0.2;
/// Overlay opacity.
pub const OVERLAY_ALPHA: f32 = 0.35;

/// Linear RGB per designation kind (the designation tools' colours).
fn kind_rgb(kind: DesignationKind) -> [f32; 3] {
    match kind {
        DesignationKind::Dig => [1.0, 0.48, 0.03],
        DesignationKind::Deconstruct => [1.0, 0.07, 0.05],
        DesignationKind::Build(_) => [0.32, 0.6, 1.0],
        DesignationKind::Chop => [0.26, 0.79, 0.1],
    }
}

/// Which colour group a kind is drawn in (tiles of one group merge into runs).
fn group(kind: DesignationKind) -> u8 {
    match kind {
        DesignationKind::Dig => 0,
        DesignationKind::Deconstruct => 1,
        DesignationKind::Build(_) => 2,
        DesignationKind::Chop => 3,
    }
}

/// The level above which the cut hides designations: the working level while
/// the cut is on, `None` (nothing hidden) otherwise.
pub fn overlay_top_level(working: &WorkingLevel) -> Option<i32> {
    working.level.filter(|_| working.cut)
}

/// The designated tiles as boxes: per kind, x-runs of tiles on one `(y, z)` row.
/// Tiles above `top_level` (the cut — [`overlay_top_level`]) are left out.
pub fn designation_boxes(
    designations: &BTreeMap<TilePos, Designation>,
    top_level: Option<i32>,
) -> Vec<(TileBox, DesignationKind)> {
    let mut by_group: BTreeMap<u8, (DesignationKind, Vec<TilePos>)> = BTreeMap::new();
    for (p, d) in designations {
        if top_level.is_some_and(|top| p[1] > top) {
            continue;
        }
        by_group.entry(group(d.kind)).or_insert_with(|| (d.kind, Vec::new())).1.push(*p);
    }
    let mut out = Vec::new();
    for (_, (kind, mut tiles)) in by_group {
        tiles.sort_unstable_by_key(|p| (p[1], p[2], p[0]));
        out.extend(merge_runs(tiles.into_iter()).into_iter().map(|b| (b, kind)));
    }
    out.truncate(MAX_OVERLAY_BOXES);
    out
}

/// Positions, colours and indices of the boxes, in region-local sim units on
/// terrain `kind` (tile sizes through `TerrainKind`).
pub fn overlay_mesh_data(kind: TerrainKind, boxes: &[(TileBox, DesignationKind)]) -> (Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<u32>) {
    let size = Vec3::new(kind.tile_width_units(), kind.tile_height_units(), kind.tile_width_units());
    let (mut positions, mut colors, mut indices) = (Vec::new(), Vec::new(), Vec::new());
    for (b, k) in boxes {
        let lo = Vec3::new(b.min[0] as f32, b.min[1] as f32, b.min[2] as f32) * size - Vec3::splat(OVERLAY_INFLATE);
        let hi = Vec3::new(b.max[0] as f32 + 1.0, b.max[1] as f32 + 1.0, b.max[2] as f32 + 1.0) * size + Vec3::splat(OVERLAY_INFLATE);
        let [r, g, bl] = kind_rgb(*k);
        let base = positions.len() as u32;
        for i in 0..8u32 {
            let pick = |bit: u32, a: f32, b: f32| if i >> bit & 1 == 1 { b } else { a };
            positions.push([pick(0, lo.x, hi.x), pick(1, lo.y, hi.y), pick(2, lo.z, hi.z)]);
            colors.push([r, g, bl, OVERLAY_ALPHA]);
        }
        // Corner i = x | y << 1 | z << 2; two triangles per face, outward.
        const FACES: [[u32; 4]; 6] =
            [[1, 3, 7, 5], [0, 4, 6, 2], [2, 6, 7, 3], [0, 1, 5, 4], [4, 5, 7, 6], [0, 2, 3, 1]];
        for f in FACES {
            indices.extend([f[0], f[1], f[2], f[0], f[2], f[3]].map(|v| base + v));
        }
    }
    (positions, colors, indices)
}

/// The overlay entity and what it was last built from.
#[derive(Default)]
pub struct OverlayBuilt {
    entity: Option<Entity>,
    key: Option<(RegionId, u64, Option<i32>)>,
    material: Option<Handle<StandardMaterial>>,
}

/// Keep the overlay mesh on the home stud region's designations.
pub fn update_stud_overlay(
    mut commands: Commands,
    mirror: Res<TileMirror>,
    working: Res<WorkingLevel>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut built: crate::session::SessionLocal<OverlayBuilt>,
) {
    let built = built.get();
    let top_level = overlay_top_level(&working);
    let key = mirror.region().filter(|_| mirror.terrain() == TerrainKind::Stud).map(|r| (r, mirror.version(), top_level));
    if built.key == key {
        return;
    }
    built.key = key;
    if let Some(e) = built.entity.take() {
        commands.entity(e).try_despawn();
    }
    let Some((region, _, _)) = key else { return };
    let boxes = designation_boxes(mirror.designations(), top_level);
    if boxes.is_empty() {
        return;
    }
    let (positions, colors, indices) = overlay_mesh_data(TerrainKind::Stud, &boxes);
    let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
        .with_inserted_indices(Indices::U32(indices));
    let material = built
        .material
        .get_or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: Color::WHITE,
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                cull_mode: None,
                ..default()
            })
        })
        .clone();
    let e = commands
        .spawn((
            Name::new("stud designations"),
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(material),
            Transform::from_translation(Vec3::from_array(region.world_offset())),
            Visibility::default(),
            NotShadowCaster,
            NotShadowReceiver,
            SessionScoped,
        ))
        .id();
    built.entity = Some(e);
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{OpId, DEFAULT_PRIORITY};

    fn d(kind: DesignationKind) -> Designation {
        Designation { kind, priority: DEFAULT_PRIORITY, op: OpId { client: 1, seq: 0 } }
    }

    #[test]
    fn runs_merge_per_kind_and_boxes_are_tile_sized() {
        let mut m = BTreeMap::new();
        for x in 0..4 {
            m.insert([x, 5, 2], d(DesignationKind::Dig));
        }
        m.insert([4, 5, 2], d(DesignationKind::Chop));
        m.insert([0, 6, 2], d(DesignationKind::Dig));
        let boxes = designation_boxes(&m, None);
        assert_eq!(boxes.len(), 3, "a run of four digs, a dig one level up, a chop");
        assert_eq!(boxes[0].0, TileBox { min: [0, 5, 2], max: [3, 5, 2] });
        let kind = TerrainKind::Stud;
        let (positions, colors, indices) = overlay_mesh_data(kind, &boxes[..1]);
        assert_eq!((positions.len(), colors.len(), indices.len()), (8, 8, 36));
        let (w, h) = (kind.tile_width_units(), kind.tile_height_units());
        let lo = positions.iter().fold([f32::MAX; 3], |a, p| [a[0].min(p[0]), a[1].min(p[1]), a[2].min(p[2])]);
        let hi = positions.iter().fold([f32::MIN; 3], |a, p| [a[0].max(p[0]), a[1].max(p[1]), a[2].max(p[2])]);
        assert_eq!(lo, [-OVERLAY_INFLATE, 5.0 * h - OVERLAY_INFLATE, 2.0 * w - OVERLAY_INFLATE]);
        assert_eq!(hi, [4.0 * w + OVERLAY_INFLATE, 6.0 * h + OVERLAY_INFLATE, 3.0 * w + OVERLAY_INFLATE]);
        assert!(h > w, "stud tiles are taller than wide: 1.2 m levels");
    }

    /// With the cut on, designations above the working level are hidden and
    /// those on it are drawn whole — their box reaches over the cut plane.
    #[test]
    fn the_cut_hides_designations_above_the_working_level_only() {
        let mut m = BTreeMap::new();
        for y in 4..=7 {
            m.insert([0, y, 0], d(DesignationKind::Dig));
        }
        let kind = TerrainKind::Stud;
        let on = WorkingLevel { level: Some(5), cut: true, terrain: kind };
        assert_eq!(overlay_top_level(&on), Some(5));
        let boxes = designation_boxes(&m, overlay_top_level(&on));
        let levels: Vec<i32> = boxes.iter().map(|(b, _)| b.min[1]).collect();
        assert_eq!(levels, vec![4, 5], "levels 6 and 7 are cut away");
        // The working level's box is whole: its top stands over the cut plane.
        let cut = crate::renderer::commander::level::cut_y(&on, Some(game::RegionCoords::new(0, 0).into())).unwrap();
        let (positions, _, _) = overlay_mesh_data(kind, &boxes[1..]);
        let top = positions.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
        assert_eq!(top, cut + OVERLAY_INFLATE);
        // Cut off, or no level yet: everything.
        let off = WorkingLevel { level: Some(5), cut: false, terrain: kind };
        assert_eq!(overlay_top_level(&off), None);
        assert_eq!(designation_boxes(&m, overlay_top_level(&off)).len(), 4);
        assert_eq!(overlay_top_level(&WorkingLevel { level: None, cut: true, terrain: kind }), None);
    }
}
