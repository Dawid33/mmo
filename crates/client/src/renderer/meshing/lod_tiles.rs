//! Merged LOD tiles, on every backend — the LOD-ring counterpart of `chunk_mesh`.
//!
//! A LOD root (one `LodKey`) has one child per 32³ cell group, each meshed on
//! its own (`LodMeshTask`); drawn one by one that is thousands of draws on
//! WebGL and thousands of arena slots on the pulled path. So a meshed cell
//! keeps its quad records ([`BrickRecords`]) and marks itself [`LodTileDirty`];
//! [`rebuild_lod_tiles`] merges the cells of each [`LOD_TILE_GROUPS`]³ group
//! tile of a root into one record list (`records::merge_records`, each cell
//! placed by its group within the tile) on a tile entity (a child of the root
//! at the tile's origin, scaled by the key's factor, like the cells): its
//! `ChunkQuads` on the pulled path, a mesh expanded from them on `PerChunk`
//! (WebGL2). A tile is built once none of its cells is still queued or
//! meshing (cells never change in place: a new payload is a new root), then
//! its cells count as `LodRasterReady`.
//!
//! A cell with no `LodGroup` (headless tests) draws its own records.
//!
//! Spec: docs/superpowers/specs/2026-11-02-webgl-chunk-meshes-investigation.md.

use std::collections::{BTreeMap, BTreeSet};

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoAutoAabb;
use bevy::math::Vec3A;
use bevy::pbr::{ExtendedMaterial, MeshMaterial3d, StandardMaterial};
use bevy::prelude::*;
use game::BRICK_VOXELS;

use super::super::voxel_material::StandardVoxelMaterial;
use super::chunk_mesh::{per_chunk_backend, records_to_mesh_data, BrickRecords};
use super::super::config::TerrainRenderConfig;
use super::super::voxel_pull::ChunkQuads;
use super::records;
use super::lod::{FarFrameUploads, LodBuildQueue, LodChunk, LodMeshTask, LodRasterReady};
use super::queue::MeshBudgets;
use super::ChunkMaterial;

/// Cell groups per tile edge: a tile merges up to this many cubed cells.
pub const LOD_TILE_GROUPS: i32 = 8;

/// A LOD cell's group coordinates within its root (`spawn_lod_root`).
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LodGroup(pub [i32; 3]);

/// On a LOD cell whose tile's merged mesh is out of date.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct LodTileDirty;

/// On a LOD root: its tile entities, by tile coordinates.
#[derive(Component, Default, Debug)]
pub struct LodTiles(pub BTreeMap<[i32; 3], Entity>);

/// A merged LOD tile mesh entity (child of its root).
#[derive(Component, Clone, Copy, Debug)]
pub struct LodTileMesh {
    pub tile: [i32; 3],
}

/// The tile a cell group falls in.
pub fn tile_of(group: [i32; 3]) -> [i32; 3] {
    group.map(|g| g.div_euclid(LOD_TILE_GROUPS))
}

/// A cell's place inside its tile (each axis 0..8): its records' brick bits.
pub fn cell_local(group: [i32; 3]) -> [u8; 3] {
    group.map(|g| g.rem_euclid(LOD_TILE_GROUPS) as u8)
}

/// Root-local transform of a tile at `factor`: the cells' own placement rule
/// (`spawn_lod_root`: `group * 32 * factor`, y lowered one cell for the skirt).
pub fn tile_transform(tile: [i32; 3], factor: u32) -> Transform {
    let span = BRICK_VOXELS as i32 * factor as i32 * LOD_TILE_GROUPS;
    Transform {
        translation: Vec3::new(
            (tile[0] * span) as f32,
            (tile[1] * span - factor as i32) as f32,
            (tile[2] * span) as f32,
        ),
        scale: Vec3::splat(factor as f32),
        ..default()
    }
}

/// Constant bounds of a tile mesh, in cells: the tile cube plus the skirted
/// cells' one-cell Y headroom (see `apply_lod_meshes`).
pub fn tile_aabb() -> Aabb {
    let half = (LOD_TILE_GROUPS * BRICK_VOXELS as i32) as f32 / 2.0;
    Aabb { center: Vec3A::new(half, half + 0.5, half), half_extents: Vec3A::new(half, half + 0.5, half) }
}

type VoxelMaterial = ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>;

/// Rebuild every dirty tile whose cells have all meshed, within the far
/// upload budget (see the module doc).
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn rebuild_lod_tiles(
    mut commands: Commands,
    dirty: Query<(&ChildOf, &LodGroup), With<LodTileDirty>>,
    children: Query<&Children>,
    cells: Query<(&LodGroup, &LodChunk, Option<&BrickRecords>, Has<LodMeshTask>, Has<LodTileDirty>)>,
    mut tiles: Query<&mut LodTiles>,
    queue: Res<LodBuildQueue>,
    mut meshes: ResMut<Assets<Mesh>>,
    material: Option<Res<ChunkMaterial>>,
    budgets: Res<MeshBudgets>,
    mut uploads: ResMut<FarFrameUploads>,
    render_config: Option<Res<TerrainRenderConfig>>,
) {
    if dirty.is_empty() {
        return;
    }
    let per_chunk = per_chunk_backend(render_config.as_deref());
    let wanted: BTreeSet<(Entity, [i32; 3])> =
        dirty.iter().map(|(parent, group)| (parent.parent(), tile_of(group.0))).collect();
    for (root, tile) in wanted {
        if uploads.0 >= budgets.uploads_per_frame {
            break;
        }
        let Ok(kids) = children.get(root) else { continue };
        let members: Vec<Entity> =
            kids.iter().filter(|&c| cells.get(c).is_ok_and(|(g, ..)| tile_of(g.0) == tile)).collect();
        let settled = members
            .iter()
            .all(|&c| !queue.contains_raster(c) && cells.get(c).is_ok_and(|(_, _, _, meshing, _)| !meshing));
        if !settled {
            continue;
        }
        let Some(factor) = members.first().and_then(|&c| cells.get(c).ok()).map(|(_, lod, ..)| lod.factor) else {
            continue;
        };
        let parts: Vec<([u8; 3], std::sync::Arc<Vec<records::QuadRecord>>)> = members
            .iter()
            .filter_map(|&c| {
                let (group, _, recs, _, _) = cells.get(c).ok()?;
                Some((cell_local(group.0), recs?.0.clone()))
            })
            .collect();
        let merged = records::merge_records(parts.iter().map(|(local, r)| (*local, r.as_slice())));
        for &c in &members {
            commands.entity(c).remove::<LodTileDirty>().insert(LodRasterReady);
        }
        let Ok(mut root_tiles) = tiles.get_mut(root) else { continue };
        let entity = match root_tiles.0.get(&tile) {
            Some(&e) => e,
            None if merged.is_empty() => continue,
            None => {
                let e = commands
                    .spawn((tile_transform(tile, factor), Visibility::default(), LodTileMesh { tile }, ChildOf(root)))
                    .id();
                root_tiles.0.insert(tile, e);
                e
            }
        };
        let mut ec = commands.entity(entity);
        if merged.is_empty() {
            ec.remove::<(Mesh3d, MeshMaterial3d<VoxelMaterial>, ChunkQuads)>();
            continue;
        }
        uploads.0 += 1;
        ec.insert((tile_aabb(), NoAutoAabb));
        if per_chunk {
            let data = records_to_mesh_data(&merged).expect("non-empty records mesh");
            ec.insert(Mesh3d(meshes.add(data.into_mesh())));
            if let Some(material) = &material {
                ec.insert(MeshMaterial3d(material.0.clone()));
            }
        } else {
            ec.insert(ChunkQuads(std::sync::Arc::new(merged)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_partition_groups_and_offsets_stay_in_nine_bits() {
        assert_eq!(tile_of([0, 0, 0]), [0, 0, 0]);
        assert_eq!(tile_of([7, 8, -1]), [0, 1, -1]);
        assert_eq!(cell_local([7, 8, -1]), [7, 0, 7]);
        // Largest position a cell can reach inside its tile: 7 * 32 + 33.
        assert!(7 * BRICK_VOXELS as u32 + 33 <= super::super::pack::POS_MAX);
    }

    #[test]
    fn a_tile_sits_where_its_first_cell_would() {
        for factor in [2u32, 4, 16, 256] {
            for tile in [[0, 0, 0], [1, -1, 2]] {
                let group = tile.map(|t| t * LOD_TILE_GROUPS);
                let span = 32 * factor as i32;
                let cell = Vec3::new(
                    (group[0] * span) as f32,
                    (group[1] * span - factor as i32) as f32,
                    (group[2] * span) as f32,
                );
                assert_eq!(tile_transform(tile, factor).translation, cell);
                assert_eq!(tile_transform(tile, factor).scale, Vec3::splat(factor as f32));
            }
        }
    }
}
