use std::collections::{BTreeMap, BTreeSet};

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoAutoAabb;
use bevy::math::Vec3A;
use bevy::mesh::Mesh;
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};
use game::Voxel;
pub use game::lod::{build_far_lod_cells, downsample2, LodCell};

use super::super::block_textures::BlockTextureLayers;
use super::super::voxel_material::StandardVoxelMaterial;
use super::binary::{self, Face, Quad};
use super::emit;
use super::neighbors::gather_padded;
use super::queue::{FrameUploads, MeshBudgets};
use super::records;
use super::{ChunkMaterial, MeshOutput, MeshingTask};
#[cfg(test)]
use super::pack;

/// Does `q`'s plane sit on the 32-grid's outer edge along its own face axis?
/// Only X/Z side faces are ever skirted — Y (top/bottom) faces never are.
fn is_border_side_quad(q: &Quad) -> bool {
    match q.face {
        Face::NegX => q.min[0] == 0,
        Face::PosX => q.min[0] == 31, // positive face plane sits at min+1 == 32
        Face::NegZ => q.min[2] == 0,
        Face::PosZ => q.min[2] == 31,
        _ => false,
    }
}

/// Extend a border side-quad one cell downward: undo one unit of the
/// caller's +1 Y-bias (`min[1] -= 1`) and grow whichever extent maps to Y for
/// this face's axis. Per `cross_axes` (binary.rs): X faces have (U=Y, V=Z),
/// so Y rides `w`; Z faces have (U=X, V=Y), so Y rides `h`. Y faces (axis 1)
/// are never skirted — callers must not pass one.
fn skirt_extend(q: &mut Quad) {
    debug_assert_ne!(q.face.axis(), 1, "Y faces are never skirted");
    q.min[1] -= 1;
    match q.face.axis() {
        0 => q.w += 1,
        2 => q.h += 1,
        _ => unreachable!("skirt_extend called on a non-side face"),
    }
}

/// Gather padding from same-scale neighbors, greedy mesh, bias every quad up
/// by one cell (`min[1] += 1`), then skirt border side-quads one cell back
/// down to the true floor so adjacent LOD rings (or ring boundaries at
/// different scales) never show a seam gap at y=0. Resulting quads span Y
/// range 0..=33 — callers translate `Transform.y` down by the scale factor
/// to compensate (Task 3). Shared by `mesh_lod_grid` and `mesh_lod_output`.
fn lod_quads(grid: &[Voxel], neighbors: &BTreeMap<[i32; 3], &[Voxel]>) -> Vec<Quad> {
    let padded = gather_padded(grid, neighbors);
    let mut quads = binary::mesh_quads(&padded);
    for q in &mut quads {
        q.min[1] += 1;
    }
    for q in &mut quads {
        if q.face.axis() != 1 && is_border_side_quad(q) {
            skirt_extend(q);
        }
    }
    quads
}

/// Mesh a coarse LOD grid. See `lod_quads` for the bias/skirt contract.
pub fn mesh_lod_grid(
    grid: &[Voxel],
    neighbors: &BTreeMap<[i32; 3], &[Voxel]>,
    layers: &BlockTextureLayers,
) -> Option<Mesh> {
    emit::quads_to_mesh(&lod_quads(grid, neighbors), layers)
}

/// Mesh OR records (per `pull`) for a coarse LOD grid — the pull-aware
/// counterpart to `mesh_lod_grid`, used by `spawn_lod_mesh_tasks`.
/// `connectivity` is always 0: LOD chunks never carry a connectivity mask
/// (the field only exists so `MeshOutput` can be shared with full-res).
fn mesh_lod_output(
    grid: &[Voxel],
    neighbors: &BTreeMap<[i32; 3], &[Voxel]>,
    layers: &BlockTextureLayers,
    pull: bool,
) -> MeshOutput {
    let quads = lod_quads(grid, neighbors);
    let (mesh, records) = if pull {
        let r = records::quads_to_records(&quads, layers);
        (None, (!r.is_empty()).then_some(r))
    } else {
        (emit::quads_to_mesh(&quads, layers), None)
    };
    MeshOutput { mesh, records, connectivity: 0 }
}

/// Scale factor of this LOD chunk relative to a full-res chunk (2, 4, 8, ...).
/// Distinguishes coarse LOD render entities from full-res chunks; LOD chunks
/// never carry `ChunkKey` and never enter the world-keyed `ChunkGrid`.
#[derive(Component, Copy, Clone)]
pub struct LodChunk {
    pub factor: u32,
}

/// The 32³ coarse voxel grid for one LOD chunk cell, same layout contract as
/// full-res `VoxelData` (`x + 32*y + 32*32*z`).
#[derive(Component)]
pub struct LodVoxelData(pub Vec<Voxel>);

/// Optional same-batch neighbor refs for padding gather, set by the spawner.
/// Entities named here must be LOD siblings at the same `factor` — offsets
/// follow the same `[-1,0,1]³` face-direction convention as `gather_padded`.
#[derive(Component, Default)]
pub struct LodNeighbors(pub BTreeMap<[i32; 3], Entity>);

/// In-flight background LOD mesh build, spawned by `spawn_lod_mesh_tasks` and
/// polled to completion by `apply_lod_meshes`.
#[derive(Component)]
pub struct LodMeshTask(Task<MeshOutput>);

/// Drain changed `LodVoxelData` into mesh tasks, FIFO (no distance ranking —
/// LOD is inherently low-priority background work), sharing `MeshBudgets.max_in_flight`
/// with full-res: full-res never even looks at LOD's in-flight count (it always
/// wins), so LOD's own capacity here is the budget minus BOTH full-res's and its
/// own in-flight tasks — this keeps LOD from pushing the combined count over the
/// cap on its own (full-res can still transiently exceed it by design, since it
/// always wins and never defers to LOD).
///
/// The `Local` queue (not a bare `Changed<LodVoxelData>` filter alone) matters:
/// Bevy consumes a system's change-detection window every time it runs, so an
/// entity skipped this frame by the budget cap would silently drop out of
/// `Changed<>` on the next run if we didn't enqueue every changed entity,
/// unconditionally, before applying the cap.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn spawn_lod_mesh_tasks(
    mut commands: Commands,
    mut queue: Local<BTreeSet<Entity>>,
    budgets: Res<MeshBudgets>,
    full_res_in_flight: Query<(), With<MeshingTask>>,
    lod_in_flight: Query<(), With<LodMeshTask>>,
    changed: Query<Entity, Changed<LodVoxelData>>,
    grids: Query<(&LodVoxelData, Option<&LodNeighbors>)>,
    layers: Res<BlockTextureLayers>,
    path: Option<Res<super::super::voxel_pull::RenderPath>>,
) {
    for e in changed.iter() {
        queue.insert(e);
    }
    queue.retain(|&e| grids.contains(e));
    if queue.is_empty() {
        return;
    }
    let capacity = budgets
        .max_in_flight
        .saturating_sub(full_res_in_flight.iter().count() + lod_in_flight.iter().count());
    if capacity == 0 {
        return;
    }
    let pull = path.map_or(super::super::voxel_pull::DEFAULT_PULL_PATH, |p| p.pull);

    let pool = AsyncComputeTaskPool::get();
    let batch: Vec<Entity> = queue.iter().copied().take(capacity).collect();
    for e in batch {
        queue.remove(&e);
        let Ok((voxels, lod_neighbors)) = grids.get(e) else { continue };
        let mut neigh: BTreeMap<[i32; 3], Vec<Voxel>> = BTreeMap::new();
        if let Some(ln) = lod_neighbors {
            for (&off, &ne) in &ln.0 {
                if let Ok((nv, _)) = grids.get(ne) {
                    neigh.insert(off, nv.0.clone());
                }
            }
        }
        let grid = voxels.0.clone();
        let layers = layers.0.clone();
        let task = pool.spawn(async move {
            let neighbor_refs: BTreeMap<[i32; 3], &[Voxel]> =
                neigh.iter().map(|(&k, v)| (k, v.as_slice())).collect();
            mesh_lod_output(&grid, &neighbor_refs, &BlockTextureLayers(layers), pull)
        });
        commands.entity(e).insert(LodMeshTask(task));
    }
}

/// Poll in-flight `LodMeshTask`s to completion and attach the resulting
/// `Mesh3d`, up to the leftover upload budget after full-res applies
/// (`FrameUploads`, shared with `apply_meshed_chunks`, which runs first in
/// the chain and always claims its uploads before this system sees the
/// counter). Inserts the shared `ChunkMaterial`, a biased `Aabb` covering the
/// mesh's `0..=33` Y range, and `NoAutoAabb` — no `ChunkKey`, no
/// `ChunkConnectivity`, no grid entry: LOD chunks render but stay outside the
/// world-keyed grid and the cave-culling BFS entirely.
pub fn apply_lod_meshes(
    mut commands: Commands,
    mut tasks: Query<(Entity, &mut LodMeshTask)>,
    mut meshes: ResMut<Assets<Mesh>>,
    material: Option<Res<ChunkMaterial>>,
    budgets: Res<MeshBudgets>,
    mut uploads: ResMut<FrameUploads>,
) {
    for (e, mut task) in &mut tasks {
        if uploads.0 >= budgets.uploads_per_frame {
            break; // remaining completed tasks attach next frame
        }
        let Some(MeshOutput { mesh, records, .. }) = block_on(future::poll_once(&mut task.0)) else { continue };
        uploads.0 += 1;
        let mut ec = commands.entity(e);
        ec.remove::<LodMeshTask>();
        if let Some(records) = records {
            ec.insert((
                super::super::voxel_pull::ChunkQuads(std::sync::Arc::new(records)),
                // Skirted LOD meshes span y = 0..=33 (see `lod_quads`'s doc
                // comment): biased up one cell from the standard 0..32 cube
                // plus the +1 unbias headroom, so the Aabb is offset and
                // grown by half a cell on Y relative to the full-res chunk's
                // constant Aabb.
                Aabb {
                    center: Vec3A::new(16.0, 16.5, 16.0),
                    half_extents: Vec3A::new(16.0, 16.5, 16.0),
                },
                NoAutoAabb,
            ));
            continue;
        }
        match mesh {
            Some(mesh) => {
                ec.insert((
                    Mesh3d(meshes.add(mesh)),
                    // See the pull-path Aabb comment above for the y-bias rationale.
                    Aabb {
                        center: Vec3A::new(16.0, 16.5, 16.0),
                        half_extents: Vec3A::new(16.0, 16.5, 16.0),
                    },
                    NoAutoAabb,
                ));
                if let Some(material) = &material {
                    ec.insert(MeshMaterial3d(material.0.clone()));
                }
            }
            None => {
                ec.remove::<(
                    Mesh3d,
                    MeshMaterial3d<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>,
                    super::super::voxel_pull::ChunkQuads,
                )>();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{BlockId, CHUNK_VOXEL_COUNT};

    fn base_quad(face: Face) -> Quad {
        Quad { min: [10, 20, 30], w: 4, h: 7, face, block: BlockId(1), ao: [3; 4] }
    }

    #[test]
    fn skirt_extend_grows_the_y_mapped_extent_per_face() {
        // X faces: cross_axes(0) = (U=1/Y, V=2/Z) — Y rides `w`.
        for face in [Face::NegX, Face::PosX] {
            let mut q = base_quad(face);
            skirt_extend(&mut q);
            assert_eq!(q.min, [10, 19, 30], "{face:?}: min[1] decrements by one");
            assert_eq!((q.w, q.h), (5, 7), "{face:?}: w (Y-mapped) grows, h untouched");
        }
        // Z faces: cross_axes(2) = (U=0/X, V=1/Y) — Y rides `h`.
        for face in [Face::NegZ, Face::PosZ] {
            let mut q = base_quad(face);
            skirt_extend(&mut q);
            assert_eq!(q.min, [10, 19, 30], "{face:?}: min[1] decrements by one");
            assert_eq!((q.w, q.h), (4, 8), "{face:?}: h (Y-mapped) grows, w untouched");
        }
    }

    #[test]
    fn skirt_extend_leaves_other_fields_untouched() {
        let mut q = base_quad(Face::PosX);
        skirt_extend(&mut q);
        assert_eq!(q.min[0], 10, "non-Y min components untouched");
        assert_eq!(q.min[2], 30, "non-Y min components untouched");
        assert_eq!(q.block, BlockId(1));
        assert_eq!(q.ao, [3; 4]);
    }

    #[test]
    #[should_panic]
    fn skirt_extend_rejects_y_faces_in_debug() {
        // Y faces are never side-skirted; calling this on one is a caller bug.
        let mut q = base_quad(Face::PosY);
        skirt_extend(&mut q);
    }

    #[test]
    fn is_border_side_quad_matches_the_four_side_faces_at_the_grid_edge() {
        // Border cases.
        assert!(is_border_side_quad(&Quad { min: [0, 5, 5], ..base_quad(Face::NegX) }));
        assert!(is_border_side_quad(&Quad { min: [31, 5, 5], ..base_quad(Face::PosX) }));
        assert!(is_border_side_quad(&Quad { min: [5, 5, 0], ..base_quad(Face::NegZ) }));
        assert!(is_border_side_quad(&Quad { min: [5, 5, 31], ..base_quad(Face::PosZ) }));
        // Interior (one cell in from the edge): not a border quad.
        assert!(!is_border_side_quad(&Quad { min: [1, 5, 5], ..base_quad(Face::NegX) }));
        assert!(!is_border_side_quad(&Quad { min: [30, 5, 5], ..base_quad(Face::PosX) }));
        assert!(!is_border_side_quad(&Quad { min: [5, 5, 1], ..base_quad(Face::NegZ) }));
        assert!(!is_border_side_quad(&Quad { min: [5, 5, 30], ..base_quad(Face::PosZ) }));
        // Y faces are never border side-quads, even sitting on the top/bottom plane.
        assert!(!is_border_side_quad(&Quad { min: [5, 0, 5], ..base_quad(Face::NegY) }));
        assert!(!is_border_side_quad(&Quad { min: [5, 31, 5], ..base_quad(Face::PosY) }));
    }

    fn packed_of(mesh: &Mesh) -> Vec<pack::UnpackedVertex> {
        use bevy::mesh::VertexAttributeValues;
        let VertexAttributeValues::Uint32x2(packed) = mesh
            .attribute(crate::renderer::voxel_material::ATTRIBUTE_VOXEL_PACKED)
            .unwrap()
        else {
            panic!("packed attribute is Uint32x2")
        };
        packed.iter().map(|&p| pack::unpack(p)).collect()
    }

    #[test]
    fn skirted_mesh_shifts_up_and_extends_border_side_faces_down() {
        // Full-solid 32^3 grid, no neighbors (air shell): every outer face is
        // visible and each of the 6 faces merges to one quad.
        let voxels = vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT];
        let neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        let mesh = mesh_lod_grid(&voxels, &neighbors, &BlockTextureLayers::default())
            .expect("solid grid meshes");
        let verts = packed_of(&mesh);

        for face in [Face::NegX, Face::PosX, Face::NegZ, Face::PosZ] {
            let ys: Vec<u32> = verts.iter().filter(|v| v.face == face).map(|v| v.pos[1]).collect();
            assert!(!ys.is_empty(), "{face:?} must be present");
            assert_eq!(ys.iter().copied().min(), Some(0), "{face:?}: skirt reaches y=0");
            assert_eq!(ys.iter().copied().max(), Some(33), "{face:?}: biased top reaches y=33");
        }
        let top_ys: Vec<u32> =
            verts.iter().filter(|v| v.face == Face::PosY).map(|v| v.pos[1]).collect();
        assert!(!top_ys.is_empty());
        assert!(top_ys.iter().all(|&y| y == 33), "top face sits at biased y=33, not skirted");

        let bottom_ys: Vec<u32> =
            verts.iter().filter(|v| v.face == Face::NegY).map(|v| v.pos[1]).collect();
        assert!(!bottom_ys.is_empty());
        assert!(bottom_ys.iter().all(|&y| y == 1), "bottom face sits at biased y=1, not skirted");
    }

    #[test]
    fn interior_side_faces_are_not_skirted() {
        // A lone solid cell away from every grid edge: its side faces must
        // stay at the biased unit span, with no skirt reaching y=0.
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        voxels[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        let mesh = mesh_lod_grid(&voxels, &neighbors, &BlockTextureLayers::default())
            .expect("lone voxel meshes");
        let verts = packed_of(&mesh);

        for face in [Face::NegX, Face::PosX, Face::NegZ, Face::PosZ] {
            let ys: Vec<u32> = verts.iter().filter(|v| v.face == face).map(|v| v.pos[1]).collect();
            assert!(!ys.is_empty(), "{face:?} must be present");
            assert_eq!(ys.iter().copied().min(), Some(6), "{face:?}: biased, unextended");
            assert_eq!(ys.iter().copied().max(), Some(7), "{face:?}: biased, unextended");
            assert!(ys.iter().all(|&y| y != 0), "{face:?}: no skirt geometry at y=0");
        }
    }

    // --- Entity plumbing: components, queue, apply (Task 3) ---------------
    //
    // These run the real production chain (`pipeline_app`, which now also
    // registers the two LOD systems ahead of `cull_chunks`) against a real
    // LOD chunk entity, per the spawner contract: children of a region root,
    // `Transform { translation: local_origin - Y*factor, scale: splat(factor) }`
    // plus `(LodChunk, LodVoxelData, Visibility::default())`.

    use super::super::queue::tests::{pipeline_app, spawn_chunk, voxels_with, wait_until};
    use super::super::queue::{ChunkGrid, ChunkKey, MeshBudgets};
    use super::super::visibility::ChunkConnectivity;

    /// A solid floor at y=0 across the whole 32³ grid: enough content for
    /// `mesh_lod_grid` to produce a mesh, simple enough to reason about.
    fn floor_grid() -> Vec<Voxel> {
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for z in 0..32 {
            for x in 0..32 {
                v[x + 32 * 32 * z] = Voxel::new(BlockId(1));
            }
        }
        v
    }

    /// Spawns a region root plus one LOD chunk child, exactly per the
    /// spawner contract Tasks 4-5 will follow.
    fn spawn_lod_chunk(
        app: &mut App,
        root_at: Vec3,
        local_origin: Vec3,
        factor: u32,
        grid: Vec<Voxel>,
    ) -> Entity {
        let root = app
            .world_mut()
            .spawn((Transform::from_translation(root_at), Visibility::default()))
            .id();
        app.world_mut()
            .spawn((
                Transform {
                    translation: local_origin - Vec3::Y * factor as f32,
                    scale: Vec3::splat(factor as f32),
                    ..default()
                },
                Visibility::default(),
                LodChunk { factor },
                LodVoxelData(grid),
                ChildOf(root),
            ))
            .id()
    }

    #[test]
    fn lod_chunks_mesh_and_scale_without_entering_the_grid() {
        let mut app = pipeline_app(MeshBudgets::default());

        // A camera plus a keyed full-res wall/behind pair so cull_chunks' BFS
        // has something to actively hide. Without this, an empty ChunkGrid and
        // no active camera make cull_chunks early-return (visibility.rs:166,
        // :169) before ever reaching the loop that would leave a keyless
        // entity's Visibility untouched — the Inherited assertion below would
        // then be vacuously true regardless of whether the BFS logic itself
        // respects keylessness. Positioning the wall/behind pair confirms the
        // BFS genuinely ran and culled something in this same update.
        app.world_mut().spawn((
            Camera3d::default(),
            Camera { is_active: true, ..Default::default() },
            GlobalTransform::from(Transform::from_translation(Vec3::new(8.0, 8.0, 8.0))),
        ));
        let _wall = spawn_chunk(
            &mut app,
            Vec3::ZERO,
            Vec3::new(32.0, 0.0, 0.0),
            vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT],
        );
        let behind = spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(64.0, 0.0, 0.0), voxels_with(&[[5, 5, 5]]));

        let e = spawn_lod_chunk(&mut app, Vec3::new(1000.0, 0.0, 0.0), Vec3::ZERO, 2, floor_grid());

        assert!(
            wait_until(&mut app, |app| {
                app.world().entity(e).contains::<Mesh3d>()
                    && app
                        .world()
                        .entity(behind)
                        .get::<Visibility>()
                        .is_some_and(|v| *v == Visibility::Hidden)
            }),
            "LOD chunk must mesh AND the BFS must actually hide the chunk behind the full-res wall \
             (proof the cull pass really ran, not just that it was registered)"
        );

        let tf = *app.world().entity(e).get::<Transform>().expect("has a transform");
        assert_eq!(tf.translation, Vec3::new(0.0, -2.0, 0.0), "translation biased down by factor");
        assert_eq!(tf.scale, Vec3::splat(2.0), "scaled by factor");

        let aabb = app.world().entity(e).get::<Aabb>().expect("biased Aabb attached");
        assert_eq!(aabb.center, Vec3A::new(16.0, 16.5, 16.0));
        assert_eq!(aabb.half_extents, Vec3A::new(16.0, 16.5, 16.0));
        assert!(app.world().entity(e).contains::<NoAutoAabb>());

        // Keyless assertions: the heart of this scenario. The grid is no
        // longer empty overall (the wall/behind full-res chunks key into it,
        // which is what gives the BFS something to cull) — the LOD chunk
        // specifically must never be one of its values.
        assert!(
            app.world().resource::<ChunkGrid>().0.values().all(|&v| v != e),
            "LOD chunks never enter the world-keyed ChunkGrid"
        );
        assert!(!app.world().entity(e).contains::<ChunkKey>(), "LOD chunks stay keyless");
        assert!(
            !app.world().entity(e).contains::<ChunkConnectivity>(),
            "LOD chunks carry no connectivity mask"
        );
        assert_eq!(
            *app.world().entity(e).get::<Visibility>().unwrap(),
            Visibility::Inherited,
            "cull_chunks' With<ChunkKey> filter never touches a keyless entity, \
             even while its BFS actively hides other chunks this same pass"
        );
    }

    #[test]
    fn full_res_tasks_starve_lod_tasks_not_vice_versa() {
        // uploads_per_frame: 0 keeps the full-res MeshingTask attached
        // indefinitely (apply_meshed_chunks never gets to poll it away), so
        // the single max_in_flight slot stays occupied across every
        // subsequent update — a deterministic way to observe the LOD task
        // never getting capacity, instead of racing the full-res task's
        // completion within one frame.
        let mut app = pipeline_app(MeshBudgets { max_in_flight: 1, uploads_per_frame: 0 });
        let full_res = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        let lod = spawn_lod_chunk(&mut app, Vec3::ZERO, Vec3::new(320.0, 0.0, 0.0), 2, floor_grid());

        for _ in 0..3 {
            app.update();
        }

        assert!(
            app.world().entity(full_res).contains::<MeshingTask>(),
            "full-res task holds the only slot (uploads_per_frame=0 keeps it attached)"
        );
        assert!(
            !app.world().entity(lod).contains::<LodMeshTask>()
                && !app.world().entity(lod).contains::<Mesh3d>(),
            "LOD task must never claim a slot while full-res occupies the shared budget"
        );
    }

    #[test]
    fn changed_lod_voxel_data_supersedes_an_in_flight_task() {
        let mut app = pipeline_app(MeshBudgets::default());
        let e = spawn_lod_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, 2, floor_grid());
        assert!(
            wait_until(&mut app, |app| app.world().entity(e).contains::<Mesh3d>()),
            "initial LOD mesh must land"
        );
        let old_mesh = app.world().entity(e).get::<Mesh3d>().expect("meshed").0.clone();

        let mut changed_grid = floor_grid();
        changed_grid[5 + 32 * 20 + 32 * 32 * 5] = Voxel::new(BlockId(2));
        app.world_mut().get_mut::<LodVoxelData>(e).unwrap().0 = changed_grid;

        assert!(
            wait_until(&mut app, |app| {
                app.world().entity(e).get::<Mesh3d>().is_some_and(|m| m.0 != old_mesh)
            }),
            "changed LodVoxelData must supersede the in-flight task and produce a new mesh handle"
        );
    }
}
