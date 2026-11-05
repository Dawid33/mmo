//! One render unit per SIM CHUNK, on every near backend.
//!
//! Bricks stay the MESHING unit (neighbour gather, remesh queue, burial, edits),
//! but drawing every 2 m brick on its own cost ~43k draws per 256 m region on
//! WebGL (`just brick-census`) and, on the pulled path, ~80k entities per region
//! extracted, frustum-culled per view and slotted in the GPU arena. So a meshed
//! brick of a sim chunk keeps its quads as records ([`BrickRecords`]) and marks
//! the chunk [`ChunkMeshDirty`]; [`rebuild_chunk_meshes`] merges every brick's
//! records into ONE list on the sim chunk entity (which already sits at the
//! chunk origin under the region root), each record placed at its brick
//! (`records::with_brick`, 9 bits of w1) and re-sorted for `face_ends`:
//! - pulled backends: that list IS the chunk's `ChunkQuads` (the shaders add
//!   `brick * 32` to the brick-local corner);
//! - `PerChunk` (WebGL2): it is expanded into one packed-vertex mesh
//!   ([`records_to_mesh_data`]; `pack.rs` positions are 9-bit chunk-local).
//!
//! A chunk is rebuilt only once none of its bricks is queued or meshing, so a
//! join builds each chunk about once, and an edit re-uploads one chunk a frame
//! or two later. Rebuilds share the near upload budget (`FrameUploads`),
//! nearest chunk first. A meshed entity with no `BrickOf` (headless tests,
//! voxel bodies) keeps drawing itself, as before.
//!
//! Spec: docs/superpowers/specs/2026-11-02-webgl-chunk-meshes-investigation.md.

use std::sync::Arc;

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoAutoAabb;
use bevy::math::Vec3A;
use bevy::pbr::{ExtendedMaterial, MeshMaterial3d, StandardMaterial};
use bevy::prelude::*;
use game::{BRICK_VOXELS, CHUNK_UNITS};

use super::super::bricks::{BrickOf, ChunkBricks};
use super::super::config::TerrainRenderConfig;
use super::super::voxel_material::StandardVoxelMaterial;
use super::super::voxel_pull::ChunkQuads;
use super::binary::{cross_axes, Face};
use super::emit::{quad_indices, MeshData, CORNERS};
use super::queue::{FrameUploads, MeshBudgets, RemeshQueue};
use super::records::{self, QuadRecord};
use super::{pack, ChunkMaterial, InteriorFaces, MeshingTask, TerrainQuadCount};

/// A meshed brick's quads (brick-local, unplaced), kept so its chunk's merged
/// list can be rebuilt without re-meshing (8 B/quad). Never drawn by itself.
#[derive(Component, Clone, Debug)]
pub struct BrickRecords(pub Arc<Vec<QuadRecord>>);

/// On a sim chunk entity whose merged mesh is out of date.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ChunkMeshDirty;

/// Whether the active near backend draws Bevy meshes (the per-chunk path).
pub fn per_chunk_backend(config: Option<&TerrainRenderConfig>) -> bool {
    config.is_some_and(|c| !c.near.uses_quad_records())
}

/// Constant bounds of a merged sim-chunk mesh: the chunk's local cube.
pub fn chunk_aabb() -> Aabb {
    let half = CHUNK_UNITS as f32 / 2.0;
    Aabb { center: Vec3A::splat(half), half_extents: Vec3A::splat(half) }
}

/// Expand (merged) records into one packed-vertex mesh: vertices and
/// triangles exactly `emit::quads_to_mesh_data`'s for the same quads, each
/// moved by its record's brick placement (`records::record_brick` × 32).
/// `None` = no quads.
pub fn records_to_mesh_data(records: &[QuadRecord]) -> Option<MeshData> {
    if records.is_empty() {
        return None;
    }
    let mut packed: Vec<[u32; 2]> = Vec::with_capacity(records.len() * 4);
    let mut indices: Vec<u32> = Vec::with_capacity(records.len() * 6);
    for rec in records {
        let [w0, w1] = rec.0;
        let face = Face::from_index((w0 >> 18) & 7);
        let flip = (w0 >> 29) & 1 == 1;
        let (w, h, layer) = (w1 & 63, (w1 >> 6) & 63, (w1 >> 12) & records::LAYER_MAX);
        let (ua, va) = cross_axes(face.axis());
        let brick = records::record_brick(*rec);
        let origin = [0, 1, 2].map(|a| ((w0 >> (6 * a)) & 63) + brick[a] * BRICK_VOXELS as u32);
        let base = packed.len() as u32;
        indices.extend(quad_indices(face, flip).map(|i| base + i));
        for (i, (cu, cv)) in CORNERS.into_iter().enumerate() {
            let mut pos = origin;
            pos[ua] += cu * w;
            pos[va] += cv * h;
            let ao = ((w0 >> (21 + 2 * i as u32)) & 3) as u8;
            packed.push(pack::pack(pos, face, ao, w, h, layer));
        }
    }
    Some(MeshData { packed, indices })
}

type VoxelMaterial = ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>;

/// Rebuild the merged records / mesh of every dirty sim chunk whose bricks have all
/// settled, nearest first, within the near upload budget (see the module doc).
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn rebuild_chunk_meshes(
    mut commands: Commands,
    dirty: Query<(Entity, &ChunkBricks, &GlobalTransform, Option<&super::coarse::CoarseTwin>), With<ChunkMeshDirty>>,
    bricks: Query<(&BrickOf, Option<&BrickRecords>, Has<MeshingTask>)>,
    queue: Res<RemeshQueue>,
    mut meshes: ResMut<Assets<Mesh>>,
    material: Option<Res<ChunkMaterial>>,
    budgets: Res<MeshBudgets>,
    mut uploads: ResMut<FrameUploads>,
    camera: Query<&GlobalTransform, With<Camera3d>>,
    render_config: Option<Res<TerrainRenderConfig>>,
) {
    if dirty.is_empty() {
        return;
    }
    let per_chunk = per_chunk_backend(render_config.as_deref());
    let eye = camera.iter().next().map(|t| t.translation());
    let mut order: Vec<(f32, Entity)> = dirty
        .iter()
        .map(|(e, _, tf, _)| {
            let centre = tf.transform_point(Vec3::splat(CHUNK_UNITS as f32 / 2.0));
            (eye.map_or(0.0, |eye| eye.distance_squared(centre)), e)
        })
        .collect();
    order.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, chunk) in order {
        if uploads.0 >= budgets.uploads_per_frame {
            break;
        }
        let Ok((_, list, _, twin)) = dirty.get(chunk) else { continue };
        let settled = list.0.iter().all(|&b| {
            // A despawned brick counts as settled (it just draws nothing).
            !queue.dirty.contains_key(&b) && bricks.get(b).map_or(true, |(_, _, meshing)| !meshing)
        });
        if !settled {
            continue; // rebuilt once its bricks land
        }
        let parts: Vec<([u8; 3], Arc<Vec<QuadRecord>>)> = list
            .0
            .iter()
            .filter_map(|&b| {
                let (of, records, _) = bricks.get(b).ok()?;
                Some((of.local, records?.0.clone()))
            })
            .collect();
        let merged = records::merge_records(parts.iter().map(|(local, r)| (*local, r.as_slice())));
        let mut ec = commands.entity(chunk);
        ec.remove::<ChunkMeshDirty>();
        if merged.is_empty() {
            ec.remove::<(Mesh3d, MeshMaterial3d<VoxelMaterial>, ChunkQuads, TerrainQuadCount, InteriorFaces)>();
            super::coarse::drop_coarse_twin(&mut commands, chunk, twin);
            continue;
        }
        uploads.0 += 1;
        ec.insert((chunk_aabb(), NoAutoAabb, TerrainQuadCount(merged.len() as u32)));
        if per_chunk {
            let data = records_to_mesh_data(&merged).expect("non-empty records mesh");
            ec.insert(Mesh3d(meshes.add(data.into_mesh())));
            if let Some(material) = &material {
                ec.insert(MeshMaterial3d(material.0.clone()));
            }
        } else {
            if merged.iter().any(|r| records::record_is_interior(*r)) {
                ec.insert(InteriorFaces);
            } else {
                ec.remove::<InteriorFaces>();
            }
            ec.insert(ChunkQuads(Arc::new(merged)));
            // Its coarse twin follows every rebuild (`meshing::coarse`).
            if super::coarse::coarse_twins_enabled() {
                ec.insert(super::coarse::CoarseDirty);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::block_textures::BlockTextureLayers;
    use super::super::binary::Quad;
    use super::super::{emit, records};
    use super::*;
    use game::BlockId;

    fn quads() -> Vec<Quad> {
        let mut out = Vec::new();
        for (i, face) in [Face::PosX, Face::NegX, Face::PosY, Face::NegY, Face::PosZ, Face::NegZ].into_iter().enumerate() {
            for ao in [[3, 3, 3, 3], [0, 1, 2, 3], [3, 0, 0, 3], [1, 3, 3, 0]] {
                out.push(Quad {
                    min: [i as u32, 31 - i as u32, 4],
                    w: 1 + i as u32,
                    h: 2,
                    face,
                    block: BlockId(1 + (i as u16 % 2)),
                    ao,
                    cutout: i == 3,
                    interior: false,
                });
            }
        }
        out
    }

    fn layers() -> BlockTextureLayers {
        let mut layers = BlockTextureLayers::default();
        layers.0.insert(BlockId(1), [10, 20, 30]);
        layers.0.insert(BlockId(2), [40, 50, 60]);
        layers
    }

    /// Each quad as its four unpacked corners, in triangle order — comparable
    /// across emit (quad order) and records (sorted opaque/face order).
    fn triangles(data: &MeshData) -> Vec<Vec<pack::UnpackedVertex>> {
        let mut quads: Vec<Vec<pack::UnpackedVertex>> = data
            .indices
            .chunks(6)
            .map(|tri| tri.iter().map(|&i| pack::unpack(data.packed[i as usize])).collect())
            .collect();
        quads.sort_by_key(|q| format!("{q:?}"));
        quads
    }

    #[test]
    fn records_expand_to_exactly_the_emitted_mesh() {
        let quads = quads();
        let layers = layers();
        let emitted = emit::quads_to_mesh_data(&quads, &layers).unwrap();
        let recs = records::quads_to_records(&quads, &layers);
        let expanded = records_to_mesh_data(&recs).unwrap();
        assert_eq!(expanded.packed.len(), emitted.packed.len());
        assert_eq!(triangles(&expanded), triangles(&emitted));
    }

    #[test]
    fn merged_bricks_land_at_their_brick_offset() {
        let quads = quads();
        let layers = layers();
        let recs = records::quads_to_records(&quads, &layers);
        let local = [7u8, 0, 3];
        let merged = records::merge_records([([0, 0, 0], recs.as_slice()), (local, recs.as_slice())]);
        assert_eq!(merged.len(), 2 * recs.len());
        // Expanding the placed copy alone = the brick-local mesh moved by 32 × local.
        let placed: Vec<QuadRecord> = merged.iter().copied().filter(|r| records::record_brick(*r) == [7, 0, 3]).collect();
        let moved = records_to_mesh_data(&placed).unwrap();
        let mut expected = records_to_mesh_data(&recs).unwrap();
        // `placed` keeps merge order, `recs` quads_to_records order: both sorted the same way.
        for p in &mut expected.packed {
            *p = pack::offset(*p, [224, 0, 96]);
        }
        assert_eq!(triangles(&moved), triangles(&expected));
        let max = moved.packed.iter().map(|&p| pack::unpack(p).pos[0]).max().unwrap();
        assert!(max <= CHUNK_UNITS as u32 + 1 && max > 7 * 32);
    }

    #[test]
    fn merged_lists_stay_in_face_order_with_cutout_last() {
        let recs = records::quads_to_records(&quads(), &layers());
        let merged = records::merge_records([([1, 2, 3], recs.as_slice()), ([0, 0, 0], recs.as_slice())]);
        let ends = records::face_ends(&merged);
        assert_eq!(ends[6], 0, "merged records are ordered, so face culling stays on");
        assert!(merged[ends[7] as usize..].iter().all(|r| records::record_is_cutout(*r)));
        assert!(merged[..ends[7] as usize].iter().all(|r| !records::record_is_cutout(*r)));
    }

    #[test]
    fn a_record_carries_its_brick() {
        let rec = records::quads_to_records(&quads(), &layers())[0];
        for local in [[0u8, 0, 0], [7, 7, 7], [1, 5, 2]] {
            let placed = records::with_brick(rec, local);
            assert_eq!(records::record_brick(placed), local.map(u32::from));
            assert_eq!(placed.0[0], rec.0[0]);
            assert_eq!(placed.0[1] & ((1 << records::BRICK_SHIFT) - 1), rec.0[1]);
        }
    }

    #[test]
    fn no_records_is_no_mesh() {
        assert!(records_to_mesh_data(&[]).is_none());
    }
}
