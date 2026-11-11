//! Designations shown on the terrain, brick by brick: the home region gets two
//! plain meshes rebuilt when the designations, the open drag or the cut change —
//! a translucent box per designated box of stud cells (one per BRICK: a
//! designation is stored on its brick's cells, `game::orders`), coloured by kind,
//! and a line mesh outlining each of them, so every selected brick reads as its
//! own brick. The bricks an open drag would take (`DragPreview::cells`, the sim's
//! own `designation_targets`) are drawn the same way while the box is open. A
//! build has no bricks yet: its boxes are tinted, not outlined, so the volume
//! reads as one ghost. Cheap and correct rather than pretty (stud world plan P7a);
//! a shader overlay on the stud materials may replace it later.
//!
//! **The cut.** The boxes are not drawn with the terrain's fortress material:
//! a box stands [`OVERLAY_INFLATE`] out of its cells, so the top of a box on the
//! working level pokes through the cut plane and a per-fragment cut would shave
//! it. Instead the mesh is built without what the cut hides
//! ([`designation_boxes`]' `top_level`): while the cut is on, boxes wholly above
//! the working level are left out and one reaching above it is cut off at its
//! top. Unlit, so the levels below are not darkened — a designation reads the
//! same at any depth.

use std::collections::BTreeMap;

use bevy::asset::RenderAssetUsages;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use game::{Designation, DesignationKind, Invalid, RegionId, StudBox};

use crate::renderer::commander::level::WorkingLevel;
use crate::renderer::commander::tiles::TileMirror;
use crate::renderer::fortress_view::DragPreview;
use crate::session::SessionScoped;

/// Most boxes drawn (designated bricks, then the open drag's); the rest are left out.
pub const MAX_OVERLAY_BOXES: usize = 8192;
/// How far a box stands out of its cells, sim units, so it does not z-fight
/// the brick it covers.
pub const OVERLAY_INFLATE: f32 = 0.2;
/// Overlay tint opacity.
pub const OVERLAY_ALPHA: f32 = 0.35;
/// Outline opacity.
pub const OUTLINE_ALPHA: f32 = 0.95;
/// How much lighter than its tint a brick's outline is (blend toward white).
pub const OUTLINE_LIGHTEN: f32 = 0.35;

/// Linear RGB per designation kind (the designation tools' colours).
fn kind_rgb(kind: DesignationKind) -> [f32; 3] {
    match kind {
        DesignationKind::Dig => [1.0, 0.48, 0.03],
        DesignationKind::Deconstruct => [1.0, 0.07, 0.05],
        DesignationKind::Build(_) => [0.32, 0.6, 1.0],
        DesignationKind::Chop => [0.26, 0.79, 0.1],
    }
}

/// One box the overlay draws, in region-local stud cells (`hi` exclusive).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayBox {
    pub lo: [i32; 3],
    pub hi: [i32; 3],
    pub kind: DesignationKind,
    /// A brick (outlined), not a build's volume.
    pub outlined: bool,
}

/// The level above which the cut hides designations: the working level while
/// the cut is on, `None` (nothing hidden) otherwise.
pub fn overlay_top_level(working: &WorkingLevel) -> Option<i32> {
    working.level.filter(|_| working.cut)
}

/// The boxes to draw: one per designation — a designated brick is its own box —
/// then one per brick the open drag would take (`preview`, its valid ones, of
/// `preview_kind`). Boxes wholly above `top_level` (the cut —
/// [`overlay_top_level`]) are left out; one reaching above it ends at its top.
pub fn designation_boxes(
    designations: &BTreeMap<StudBox, Designation>,
    preview: &[(StudBox, Result<(), Invalid>)],
    preview_kind: Option<DesignationKind>,
    top_level: Option<i32>,
) -> Vec<OverlayBox> {
    let cut = top_level.map(|top| top.saturating_add(1).saturating_mul(game::TILE_PLATES as i32));
    let shown = |b: &StudBox, kind: DesignationKind| {
        let (lo, mut hi) = b.cells();
        if let Some(cut) = cut {
            if lo[1] >= cut {
                return None;
            }
            hi[1] = hi[1].min(cut);
        }
        Some(OverlayBox { lo, hi, kind, outlined: !matches!(kind, DesignationKind::Build(_)) })
    };
    let drag = preview_kind
        .into_iter()
        .flat_map(|kind| preview.iter().filter(|(_, ok)| ok.is_ok()).filter_map(move |(b, _)| shown(b, kind)));
    designations.iter().filter_map(|(b, d)| shown(b, d.kind)).chain(drag).take(MAX_OVERLAY_BOXES).collect()
}

/// The corners of `b` in region-local sim units, grown by [`OVERLAY_INFLATE`]:
/// corner `i` = x | y << 1 | z << 2.
fn corners(b: &OverlayBox) -> [[f32; 3]; 8] {
    let s = game::STUD_UNITS as f32;
    let at = |c: [i32; 3]| Vec3::new(c[0] as f32 * s, game::plate_units(c[1]), c[2] as f32 * s);
    let (lo, hi) = (at(b.lo) - Vec3::splat(OVERLAY_INFLATE), at(b.hi) + Vec3::splat(OVERLAY_INFLATE));
    std::array::from_fn(|i| {
        let pick = |bit: usize, a: f32, b: f32| if i >> bit & 1 == 1 { b } else { a };
        [pick(0, lo.x, hi.x), pick(1, lo.y, hi.y), pick(2, lo.z, hi.z)]
    })
}

/// Positions, colours and indices of the boxes' translucent faces, in
/// region-local sim units.
pub fn overlay_mesh_data(boxes: &[OverlayBox]) -> (Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<u32>) {
    let (mut positions, mut colors, mut indices) = (Vec::new(), Vec::new(), Vec::new());
    for b in boxes {
        let [r, g, bl] = kind_rgb(b.kind);
        let base = positions.len() as u32;
        positions.extend(corners(b));
        colors.extend([[r, g, bl, OVERLAY_ALPHA]; 8]);
        // Two triangles per face, outward.
        const FACES: [[u32; 4]; 6] =
            [[1, 3, 7, 5], [0, 4, 6, 2], [2, 6, 7, 3], [0, 1, 5, 4], [4, 5, 7, 6], [0, 2, 3, 1]];
        for f in FACES {
            indices.extend([f[0], f[1], f[2], f[0], f[2], f[3]].map(|v| base + v));
        }
    }
    (positions, colors, indices)
}

/// The twelve edges of every outlined box as a line list (two vertices each):
/// positions and colours.
pub fn outline_mesh_data(boxes: &[OverlayBox]) -> (Vec<[f32; 3]>, Vec<[f32; 4]>) {
    // Corner pairs differing in exactly one axis.
    const EDGES: [[usize; 2]; 12] =
        [[0, 1], [2, 3], [4, 5], [6, 7], [0, 2], [1, 3], [4, 6], [5, 7], [0, 4], [1, 5], [2, 6], [3, 7]];
    let (mut positions, mut colors) = (Vec::new(), Vec::new());
    for b in boxes.iter().filter(|b| b.outlined) {
        let c = corners(b);
        let [r, g, bl] = kind_rgb(b.kind).map(|v| v + (1.0 - v) * OUTLINE_LIGHTEN);
        for [i, j] in EDGES {
            positions.extend([c[i], c[j]]);
            colors.extend([[r, g, bl, OUTLINE_ALPHA]; 2]);
        }
    }
    (positions, colors)
}

/// The overlay entities and what they were last built from.
#[derive(Default)]
pub struct OverlayBuilt {
    entities: Vec<Entity>,
    /// Home region, tile mirror version, the cut's level, the drag preview's stamp.
    key: Option<(RegionId, u64, Option<i32>, u64)>,
    material: Option<Handle<StandardMaterial>>,
}

/// Keep the overlay meshes on the home stud region's designations and open drag.
/// Rebuilt only when one of them or the cut changes.
pub fn update_stud_overlay(
    mut commands: Commands,
    mirror: Res<TileMirror>,
    working: Res<WorkingLevel>,
    preview: Option<Res<DragPreview>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut built: crate::session::SessionLocal<OverlayBuilt>,
) {
    let built = built.get();
    let top_level = overlay_top_level(&working);
    let stamp = preview.as_ref().map_or(0, |p| p.stamp);
    let key = mirror.region().map(|r| (r, mirror.version(), top_level, stamp));
    if built.key == key {
        return;
    }
    built.key = key;
    for e in built.entities.drain(..) {
        commands.entity(e).try_despawn();
    }
    let Some((region, ..)) = key else { return };
    let (cells, kind) = preview.as_ref().map_or((&[][..], None), |p| (&p.cells[..], p.kind));
    let boxes = designation_boxes(mirror.designations(), cells, kind, top_level);
    if boxes.is_empty() {
        return;
    }
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
    let (positions, colors, indices) = overlay_mesh_data(&boxes);
    let tint = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
        .with_inserted_indices(Indices::U32(indices));
    let (positions, colors) = outline_mesh_data(&boxes);
    let lines = (!positions.is_empty()).then(|| {
        Mesh::new(PrimitiveTopology::LineList, RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
            .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
    });
    for (name, mesh) in [("stud designations", Some(tint)), ("stud designation outlines", lines)] {
        let Some(mesh) = mesh else { continue };
        let e = commands
            .spawn((
                Name::new(name),
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(Vec3::from_array(region.world_offset())),
                Visibility::default(),
                NotShadowCaster,
                NotShadowReceiver,
                SessionScoped,
            ))
            .id();
        built.entities.push(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{OpId, DEFAULT_PRIORITY};

    fn d(kind: DesignationKind) -> Designation {
        Designation { kind, priority: DEFAULT_PRIORITY, op: OpId { client: 1, seq: 0 } }
    }

    fn brick(lo: [u8; 3], size: [u8; 3]) -> StudBox {
        StudBox { chunk: [0, 0, 0], lo, hi: [0, 1, 2].map(|a| lo[a] + size[a]) }
    }

    /// Every designated brick is its own box of its own cells, outlined; adjacent
    /// ones are never merged. A build's volume is tinted only.
    #[test]
    fn one_outlined_box_per_designated_brick() {
        let mut m = BTreeMap::new();
        // Two 2 x 4 bricks side by side, a plate, and a build's tile.
        m.insert(brick([0, 30, 4], [4, 3, 2]), d(DesignationKind::Dig));
        m.insert(brick([4, 30, 4], [4, 3, 2]), d(DesignationKind::Dig));
        m.insert(brick([0, 33, 4], [2, 1, 2]), d(DesignationKind::Chop));
        m.insert(StudBox::of_tile([4, 6, 4]).unwrap(), d(DesignationKind::Build(game::edit::PLACE_BLOCK)));
        let boxes = designation_boxes(&m, &[], None, None);
        assert_eq!(boxes.len(), 4, "one box per designation: {boxes:?}");
        assert_eq!(boxes.iter().filter(|b| b.outlined).count(), 3, "the bricks; not the build");
        assert_eq!((boxes[0].lo, boxes[0].hi), ([0, 30, 4], [4, 33, 6]));
        assert_eq!((boxes[2].lo, boxes[2].hi), ([4, 30, 4], [8, 33, 6]), "its neighbour, apart");
        let (positions, colors, indices) = overlay_mesh_data(&boxes);
        assert_eq!((positions.len(), colors.len(), indices.len()), (4 * 8, 4 * 8, 4 * 36));
        let (lines, line_colors) = outline_mesh_data(&boxes);
        assert_eq!((lines.len(), line_colors.len()), (3 * 24, 3 * 24), "twelve edges per brick");
        // The first box hugs its brick: 4 studs x 3 plates x 2 studs, inflated.
        let (s, i) = (game::STUD_UNITS as f32, OVERLAY_INFLATE);
        let lo = positions[..8].iter().fold([f32::MAX; 3], |a, p| [a[0].min(p[0]), a[1].min(p[1]), a[2].min(p[2])]);
        let hi = positions[..8].iter().fold([f32::MIN; 3], |a, p| [a[0].max(p[0]), a[1].max(p[1]), a[2].max(p[2])]);
        assert_eq!(lo, [-i, game::plate_units(30) - i, 4.0 * s - i]);
        assert_eq!(hi, [4.0 * s + i, game::plate_units(33) + i, 6.0 * s + i]);
        // Its outline runs along the same corners.
        assert!(lines[..24].iter().all(|p| positions[..8].contains(p)));
        assert_ne!(colors[0], colors[8], "a chop is not a dig's colour");
    }

    /// The bricks an open drag would take are drawn like designations; the ones the
    /// order refuses are not.
    #[test]
    fn the_open_drags_bricks_are_highlighted() {
        let a = brick([0, 30, 4], [4, 3, 2]);
        let b = brick([4, 30, 4], [4, 3, 2]);
        let preview = [(a, Ok(())), (b, Err(Invalid::Bedrock))];
        let boxes = designation_boxes(&BTreeMap::new(), &preview, Some(DesignationKind::Dig), None);
        assert_eq!(boxes, vec![OverlayBox { lo: [0, 30, 4], hi: [4, 33, 6], kind: DesignationKind::Dig, outlined: true }]);
        assert!(designation_boxes(&BTreeMap::new(), &preview, None, None).is_empty(), "an erase previews no bricks");
    }

    /// With the cut on, bricks above the working level are hidden, those on it are
    /// drawn whole, and a box reaching through the cut ends at it.
    #[test]
    fn the_cut_hides_bricks_above_the_working_level_only() {
        let t = game::TILE_PLATES as u8;
        let mut m = BTreeMap::new();
        for level in 4..=7u8 {
            m.insert(brick([0, level * t, 0], [2, 3, 2]), d(DesignationKind::Dig));
        }
        // A slab's dig box through levels 3..=8.
        m.insert(brick([8, 3 * t, 0], [2, 6 * t, 2]), d(DesignationKind::Dig));
        let on = WorkingLevel { level: Some(5), cut: true };
        assert_eq!(overlay_top_level(&on), Some(5));
        let boxes = designation_boxes(&m, &[], None, overlay_top_level(&on));
        let spans: Vec<(i32, i32)> = boxes.iter().map(|b| (b.lo[1], b.hi[1])).collect();
        let t = i32::from(t);
        assert_eq!(spans, vec![(4 * t, 4 * t + 3), (5 * t, 5 * t + 3), (3 * t, 6 * t)], "levels 6 and 7 are cut away");
        // The cut box's top stands over the cut plane by the inflate only.
        let cut = crate::renderer::commander::level::cut_y(&on, Some(game::RegionCoords::new(0, 0))).unwrap();
        let (positions, _, _) = overlay_mesh_data(&boxes[2..]);
        let top = positions.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
        assert!((top - (cut + OVERLAY_INFLATE)).abs() < 1e-3, "{top} vs {cut}");
        // Cut off, or no level yet: everything.
        let off = WorkingLevel { level: Some(5), cut: false };
        assert_eq!(overlay_top_level(&off), None);
        assert_eq!(designation_boxes(&m, &[], None, overlay_top_level(&off)).len(), 5);
        assert_eq!(overlay_top_level(&WorkingLevel { level: None, cut: true }), None);
    }
}
