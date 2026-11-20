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

use super::super::config::TerrainRenderConfig;
use super::super::voxel_material::StandardVoxelMaterial;
use super::super::voxel_pull::ChunkQuads;
use super::ChunkMaterial;
use super::chunk_mesh::{BrickRecords, per_chunk_backend, records_to_mesh_data};
use super::lod::{FarFrameUploads, LodBuildQueue, LodChunk, LodMeshTask, LodRasterReady};
use super::queue::MeshBudgets;
use super::records;

/// Most cell groups per tile edge (a record's brick placement holds 0..8
/// per axis): a tile merges up to this many cubed cells.
pub const LOD_TILE_GROUPS: i32 = 8;

/// Tiles of the rings a sun shadow cascade can reach are kept to about this
/// edge, metres, on the GPU-culled path.
pub const NEAR_TILE_EDGE_M: i32 = 64;
/// The coarsest factor whose ring starts within the shadow distance at any
/// region size (block scale: the first ring of the play sizes).
pub const NEAR_TILE_MAX_FACTOR: u32 = game::lod::COARSE_MIN_FACTOR;

/// Cell groups per tile edge at `factor`.
///
/// A tile is one cull unit: every view that touches its bounds draws all of
/// it. At block scale an 8-group tile is a whole 256 m region (~57k quads),
/// and each sun cascade that reached into one drew it whole — 4.9 of the
/// 7.8 ms the four cascades cost on an integrated GPU (2026-11-14). So the
/// rings near enough to cast into a cascade (`NEAR_TILE_MAX_FACTOR` and
/// finer) get tiles of about [`NEAR_TILE_EDGE_M`]; the rings beyond keep the
/// largest tile, where the slot count matters more than the cut. The
/// per-chunk backend (WebGL2: one draw per tile, CPU-culled) keeps the
/// largest tile everywhere.
pub fn tile_groups(factor: u32, per_chunk: bool) -> i32 {
    if per_chunk || factor > NEAR_TILE_MAX_FACTOR {
        return LOD_TILE_GROUPS;
    }
    // A cell group's edge in metres: 32 cells of `factor` voxels.
    let group_m = (BRICK_VOXELS as i32 * factor as i32) / game::BLOCK_VOXELS as i32;
    (NEAR_TILE_EDGE_M / group_m.max(1)).clamp(1, LOD_TILE_GROUPS)
}

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

/// The tile a cell group falls in, with `groups` cell groups per tile edge
/// ([`tile_groups`]).
pub fn tile_of(group: [i32; 3], groups: i32) -> [i32; 3] {
    group.map(|g| g.div_euclid(groups))
}

/// A cell's place inside its tile (each axis 0..`groups`): its records' brick bits.
pub fn cell_local(group: [i32; 3], groups: i32) -> [u8; 3] {
    group.map(|g| g.rem_euclid(groups) as u8)
}

/// Root-local transform of a tile at `factor`: the cells' own placement rule
/// (`spawn_lod_root`: `group * 32 * factor`, y lowered one cell for the skirt).
pub fn tile_transform(tile: [i32; 3], factor: u32, groups: i32) -> Transform {
    let span = BRICK_VOXELS as i32 * factor as i32 * groups;
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

/// Bounds of a tile mesh, in cells: the box of the cells it merged (their
/// places inside the tile, `cell_local`) plus the skirted cells' one-cell Y
/// headroom (see `apply_lod_meshes`). Tight on purpose: terrain fills one or
/// two cell groups of a tile's height, and the full cube — 256 m tall at
/// block scale — reached into shadow cascades and view frusta its quads
/// were nowhere near. `None` without cells.
pub fn tile_aabb(cells: impl IntoIterator<Item = [u8; 3]>) -> Option<Aabb> {
    let (mut lo, mut hi) = ([u8::MAX; 3], [0u8; 3]);
    let mut any = false;
    for local in cells {
        any = true;
        for axis in 0..3 {
            lo[axis] = lo[axis].min(local[axis]);
            hi[axis] = hi[axis].max(local[axis]);
        }
    }
    if !any {
        return None;
    }
    let cell = BRICK_VOXELS as f32;
    let min = Vec3A::new(
        f32::from(lo[0]) * cell,
        f32::from(lo[1]) * cell,
        f32::from(lo[2]) * cell,
    );
    let max = Vec3A::new(
        (f32::from(hi[0]) + 1.0) * cell,
        (f32::from(hi[1]) + 1.0) * cell + 1.0,
        (f32::from(hi[2]) + 1.0) * cell,
    );
    Some(Aabb {
        center: (min + max) * 0.5,
        half_extents: (max - min) * 0.5,
    })
}

type VoxelMaterial = ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>;

/// Rebuild every dirty tile whose cells have all meshed, within the far
/// upload budget (see the module doc).
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn rebuild_lod_tiles(
    mut commands: Commands,
    dirty: Query<(&ChildOf, &LodGroup), With<LodTileDirty>>,
    children: Query<&Children>,
    cells: Query<(
        &LodGroup,
        &LodChunk,
        Option<&BrickRecords>,
        Has<LodMeshTask>,
        Has<LodTileDirty>,
    )>,
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
    // A root's cells share one factor (`spawn_lod_root`), hence one tile size.
    let wanted: BTreeSet<(Entity, i32, [i32; 3])> = dirty
        .iter()
        .filter_map(|(parent, group)| {
            let root = parent.parent();
            let factor = children
                .get(root)
                .ok()?
                .iter()
                .find_map(|c| cells.get(c).ok().map(|(_, lod, ..)| lod.factor))?;
            let groups = tile_groups(factor, per_chunk);
            Some((root, groups, tile_of(group.0, groups)))
        })
        .collect();
    for (root, groups, tile) in wanted {
        if uploads.0 >= budgets.uploads_per_frame {
            break;
        }
        let Ok(kids) = children.get(root) else {
            continue;
        };
        let members: Vec<Entity> = kids
            .iter()
            .filter(|&c| {
                cells
                    .get(c)
                    .is_ok_and(|(g, ..)| tile_of(g.0, groups) == tile)
            })
            .collect();
        let settled = members.iter().all(|&c| {
            !queue.contains_raster(c) && cells.get(c).is_ok_and(|(_, _, _, meshing, _)| !meshing)
        });
        if !settled {
            continue;
        }
        let Some(factor) = members
            .first()
            .and_then(|&c| cells.get(c).ok())
            .map(|(_, lod, ..)| lod.factor)
        else {
            continue;
        };
        let parts: Vec<([u8; 3], std::sync::Arc<Vec<records::QuadRecord>>)> = members
            .iter()
            .filter_map(|&c| {
                let (group, _, recs, _, _) = cells.get(c).ok()?;
                Some((cell_local(group.0, groups), recs?.0.clone()))
            })
            .collect();
        let merged = records::merge_records(parts.iter().map(|(local, r)| (*local, r.as_slice())));
        for &c in &members {
            commands
                .entity(c)
                .remove::<LodTileDirty>()
                .insert(LodRasterReady);
        }
        let Ok(mut root_tiles) = tiles.get_mut(root) else {
            continue;
        };
        let entity = match root_tiles.0.get(&tile) {
            Some(&e) => e,
            None if merged.is_empty() => continue,
            None => {
                let e = commands
                    .spawn((
                        tile_transform(tile, factor, groups),
                        Visibility::default(),
                        LodTileMesh { tile },
                        ChildOf(root),
                    ))
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
        // Cells that meshed to nothing (buried, empty) hold no quads to bound.
        let bounds = tile_aabb(
            parts
                .iter()
                .filter(|(_, r)| !r.is_empty())
                .map(|(local, _)| *local),
        )
        .expect("merged records come from at least one cell");
        ec.insert((bounds, NoAutoAabb));
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
