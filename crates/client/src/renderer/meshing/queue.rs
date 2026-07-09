//! Remesh scheduling: world-keyed chunk grid, boundary-zone change detection,
//! and the dirty queue drained by `spawn_mesh_tasks` (task 7). All state here
//! is render-derived — never rollback-hashed.

use std::collections::{BTreeMap, BTreeSet};

use bevy::camera::primitives::{Frustum, Sphere};
use bevy::prelude::*;
use bevy::tasks::AsyncComputeTaskPool;
use game::{Voxel, CHUNK_VOXEL_COUNT};

use super::super::bridge::VoxelData;
use super::super::block_textures::BlockTextureLayers;
use super::binary::PaddedVoxels;
use super::neighbors::{gather_padded, zone_index, zone_offsets};
use super::{build_chunk_mesh, MeshingTask};

/// World-space chunk grid: key = world position / 32. Render-side spatial
/// index for neighbor lookups; rebuilt from entities, never persisted.
#[derive(Resource, Default)]
pub struct ChunkGrid(pub BTreeMap<[i32; 3], Entity>);

/// This chunk's grid key, stamped when first keyed.
#[derive(Component, Copy, Clone)]
pub struct ChunkKey(pub [i32; 3]);

/// crc32 fingerprint of each of the 26 boundary zones (indexed by
/// `zone_index`). A neighbor only needs remeshing when the zone it can see
/// actually changed — this is what makes edit-driven remeshes O(affected).
#[derive(Component)]
pub struct BoundaryZones(pub [u32; 26]);

/// Chunks awaiting a mesh task, with their grid key if known. BTreeMap for
/// deterministic drain order (entity order is arbitrary but stable).
#[derive(Resource, Default)]
pub struct RemeshQueue {
    pub dirty: BTreeMap<Entity, Option<[i32; 3]>>,
}

/// Cap on concurrently queued meshing tasks. Each task owns a ~77 KB padded
/// snapshot; the cap bounds join-burst memory and lets priority reordering
/// act on everything not yet started. Tunable.
pub const MAX_IN_FLIGHT_MESH_TASKS: usize = 64;
/// Cap on finished meshes attached per frame — bounds per-frame GPU upload
/// spikes during bursts. Tunable.
pub const MESH_UPLOADS_PER_FRAME: usize = 32;

#[derive(Resource)]
pub struct MeshBudgets {
    pub max_in_flight: usize,
    pub uploads_per_frame: usize,
}

impl Default for MeshBudgets {
    fn default() -> Self {
        Self {
            max_in_flight: MAX_IN_FLIGHT_MESH_TASKS,
            uploads_per_frame: MESH_UPLOADS_PER_FRAME,
        }
    }
}

/// Sort rank: out-of-frustum chunks after in-frustum, then by squared
/// distance to the camera. No camera (headless/startup) ranks everything 0.
fn chunk_rank(key: [i32; 3], cam: Option<(&GlobalTransform, Option<&Frustum>)>) -> u64 {
    let Some((tf, frustum)) = cam else { return 0 };
    let center = Vec3::new(
        key[0] as f32 * 32.0 + 16.0,
        key[1] as f32 * 32.0 + 16.0,
        key[2] as f32 * 32.0 + 16.0,
    );
    let dist2 = tf.translation().distance_squared(center) as u64;
    let out = frustum
        .map(|f| {
            !f.intersects_sphere(
                &Sphere { center: center.into(), radius: 16.0 * 3f32.sqrt() },
                false,
            )
        })
        .unwrap_or(false);
    ((out as u64) << 62) | dist2.min((1 << 62) - 1)
}

/// Drain the dirty queue into meshing tasks, nearest-first, up to the
/// in-flight budget. Re-inserting `MeshingTask` supersedes (cancels) any
/// prior in-flight task for that entity — same rule as the old pipeline.
pub fn spawn_mesh_tasks(
    mut commands: Commands,
    mut queue: ResMut<RemeshQueue>,
    budgets: Res<MeshBudgets>,
    grid: Res<ChunkGrid>,
    chunks: Query<&VoxelData>,
    in_flight: Query<(), With<MeshingTask>>,
    camera: Query<(&GlobalTransform, Option<&Frustum>), With<Camera3d>>,
    layers: Res<BlockTextureLayers>,
) {
    if queue.dirty.is_empty() {
        return;
    }
    // Entities that lost VoxelData while queued are stale — drop them.
    queue.dirty.retain(|e, _| chunks.contains(*e));
    let capacity = budgets.max_in_flight.saturating_sub(in_flight.iter().count());
    if capacity == 0 {
        return;
    }
    let cam = camera.iter().next();
    let mut ranked: Vec<(u64, Entity, Option<[i32; 3]>)> = queue
        .dirty
        .iter()
        .map(|(&e, &key)| (key.map_or(0, |k| chunk_rank(k, cam)), e, key))
        .collect();
    ranked.sort_unstable_by_key(|(rank, e, _)| (*rank, *e));

    let pool = AsyncComputeTaskPool::get();
    for (_, e, key) in ranked.into_iter().take(capacity) {
        queue.dirty.remove(&e);
        let Ok(voxels) = chunks.get(e) else { continue };
        let padded = match key {
            Some(k) => {
                let mut neigh: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
                for off in zone_offsets() {
                    let nk = [k[0] + off[0], k[1] + off[1], k[2] + off[2]];
                    if let Some(&ne) = grid.0.get(&nk) {
                        if let Ok(nv) = chunks.get(ne) {
                            neigh.insert(off, nv.0.as_slice());
                        }
                    }
                }
                gather_padded(&voxels.0, &neigh)
            }
            None => PaddedVoxels::from_interior(&voxels.0),
        };
        let layers = layers.0.clone();
        let task = pool.spawn(async move { build_chunk_mesh(&padded, &BlockTextureLayers(layers)) });
        commands.entity(e).insert(MeshingTask(task));
    }
}

/// Hash the 26 boundary zones of a 32³ chunk. Zones overlap on purpose: a
/// corner voxel contributes to 3 face + 3 edge + 1 corner zone.
pub fn boundary_zones(voxels: &[Voxel]) -> [u32; 26] {
    debug_assert_eq!(voxels.len(), CHUNK_VOXEL_COUNT);
    let mut zones = [0u32; 26];
    for (i, off) in zone_offsets().enumerate() {
        let range = |o: i32| -> std::ops::Range<usize> {
            match o {
                -1 => 0..1,
                1 => 31..32,
                _ => 0..32,
            }
        };
        let mut h = crc32fast::Hasher::new();
        for z in range(off[2]) {
            for y in range(off[1]) {
                for x in range(off[0]) {
                    h.update(&voxels[x + 32 * y + 32 * 32 * z].block.0.to_le_bytes());
                }
            }
        }
        zones[i] = h.finalize();
        debug_assert_eq!(zone_index(off), i);
    }
    zones
}

/// Key changed chunks into the grid, mark them dirty, and dirty any neighbor
/// whose boundary zone changed. Runs in `Update` before `spawn_mesh_tasks`.
pub fn mark_remesh_dirty(
    mut commands: Commands,
    mut grid: ResMut<ChunkGrid>,
    mut queue: ResMut<RemeshQueue>,
    mut changed: Query<
        (
            Entity,
            &VoxelData,
            Option<&Transform>,
            Option<&ChildOf>,
            Option<&ChunkKey>,
            Option<&mut BoundaryZones>,
        ),
        Changed<VoxelData>,
    >,
    roots: Query<&Transform, Without<VoxelData>>,
) {
    for (e, voxels, tf, parent, key, mut zones) in &mut changed {
        // Resolve (or compute) the grid key. Bare test entities (no parented
        // transform) stay unkeyed and skip all neighbor logic.
        let key: Option<[i32; 3]> = key.map(|k| k.0).or_else(|| {
            let (tf, parent) = (tf?, parent?);
            let root = roots.get(parent.parent()).ok()?;
            let world = root.translation + tf.translation;
            let k = [
                (world.x / 32.0).round() as i32,
                (world.y / 32.0).round() as i32,
                (world.z / 32.0).round() as i32,
            ];
            commands.entity(e).insert(ChunkKey(k));
            Some(k)
        });
        // Idempotent: also re-enters an entity that a keyed `ChunkKey` still
        // names but that `prune_chunk_grid` evicted from the grid on an
        // earlier, since-reversed VoxelData removal (cross-frame remove then
        // reinsert) — otherwise it stays treated as air by neighbors forever.
        if let Some(k) = key {
            grid.0.insert(k, e);
        }
        queue.dirty.insert(e, key);

        let new_zones = boundary_zones(&voxels.0);
        if let Some(k) = key {
            let old = zones.as_ref().map(|z| z.0);
            for (i, off) in zone_offsets().enumerate() {
                if old.is_some_and(|o| o[i] == new_zones[i]) {
                    continue;
                }
                let nk = [k[0] + off[0], k[1] + off[1], k[2] + off[2]];
                if let Some(&ne) = grid.0.get(&nk) {
                    queue.dirty.entry(ne).or_insert(Some(nk));
                }
            }
        }
        match zones.as_deref_mut() {
            Some(z) => z.0 = new_zones,
            None => {
                commands.entity(e).insert(BoundaryZones(new_zones));
            }
        }
    }
}

/// Drop grid + queue entries for chunks whose `VoxelData` is gone for good
/// (same liveness rule as `apply_meshed_chunks`: presence wins over the
/// frame's removal log, so same-frame remove+reinsert survives), and
/// re-dirty the 26 grid neighbors of each removed key: a neighbor that
/// culled its shared-plane faces against the now-dead chunk must remesh
/// with an air shell there, or the culled faces become visible holes.
pub fn prune_chunk_grid(
    mut grid: ResMut<ChunkGrid>,
    mut queue: ResMut<RemeshQueue>,
    mut removed: RemovedComponents<VoxelData>,
    still_alive: Query<(), With<VoxelData>>,
) {
    let dead: BTreeSet<Entity> =
        removed.read().filter(|e| !still_alive.contains(*e)).collect();
    if dead.is_empty() {
        return;
    }
    let mut removed_keys = Vec::new();
    grid.0.retain(|k, e| {
        if dead.contains(e) {
            removed_keys.push(*k);
            false
        } else {
            true
        }
    });
    for e in &dead {
        queue.dirty.remove(e);
    }
    for k in removed_keys {
        for off in zone_offsets() {
            let nk = [k[0] + off[0], k[1] + off[1], k[2] + off[2]];
            if let Some(&ne) = grid.0.get(&nk) {
                queue.dirty.entry(ne).or_insert(Some(nk));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::BlockId;

    fn voxels_with(cells: &[[usize; 3]]) -> Vec<Voxel> {
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for [x, y, z] in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(1));
        }
        v
    }

    #[test]
    fn boundary_zone_reacts_only_to_its_zone() {
        let a = boundary_zones(&voxels_with(&[[5, 5, 5]]));
        let b = boundary_zones(&voxels_with(&[[5, 5, 6]]));
        assert_eq!(a, b, "interior-only changes leave every zone hash equal");

        let c = boundary_zones(&voxels_with(&[[31, 5, 5]]));
        let face_px = zone_index([1, 0, 0]);
        assert_ne!(a[face_px], c[face_px], "+X face zone changed");
        let face_nx = zone_index([-1, 0, 0]);
        assert_eq!(a[face_nx], c[face_nx], "-X face zone did not");

        let d = boundary_zones(&voxels_with(&[[31, 31, 31]]));
        assert_ne!(a[zone_index([1, 1, 1])], d[zone_index([1, 1, 1])], "corner zone changed");
        assert_ne!(a[zone_index([1, 1, 0])], d[zone_index([1, 1, 0])], "edge zone changed");
        assert_ne!(a[zone_index([1, 0, 0])], d[zone_index([1, 0, 0])], "face zone changed");
    }

    fn grid_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ChunkGrid>()
            .init_resource::<RemeshQueue>()
            .add_systems(Update, (mark_remesh_dirty, prune_chunk_grid));
        app
    }

    /// Region-root + chunk child at region-local chunk coords, like the bridge builds.
    fn spawn_chunk(app: &mut App, root_at: Vec3, local: Vec3, voxels: Vec<Voxel>) -> Entity {
        let root = app
            .world_mut()
            .spawn((Transform::from_translation(root_at), Visibility::default()))
            .id();
        app.world_mut()
            .spawn((
                Transform::from_translation(local),
                Visibility::default(),
                VoxelData(voxels),
                ChildOf(root),
            ))
            .id()
    }

    #[test]
    fn chunks_key_into_the_grid_by_world_position() {
        let mut app = grid_app();
        let e = spawn_chunk(&mut app, Vec3::new(256.0, 0.0, -512.0), Vec3::new(64.0, 32.0, 0.0), voxels_with(&[]));
        app.update();
        let grid = app.world().resource::<ChunkGrid>();
        assert_eq!(grid.0.get(&[10, 1, -16]), Some(&e), "(256+64)/32, 32/32, -512/32");
        assert!(app.world().resource::<RemeshQueue>().dirty.contains_key(&e));
    }

    #[test]
    fn boundary_change_dirties_only_affected_neighbors() {
        let mut app = grid_app();
        // Two X-adjacent chunks under one root at origin.
        let a = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        let b = spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(32.0, 0.0, 0.0), voxels_with(&[]));
        app.update();
        // Drain the initial dirtiness.
        app.world_mut().resource_mut::<RemeshQueue>().dirty.clear();

        // Interior edit in `a`: nobody's boundary view changes except a itself.
        app.world_mut().get_mut::<VoxelData>(a).unwrap().0
            [6 + 32 * 6 + 32 * 32 * 6] = Voxel::new(BlockId(1));
        app.update();
        {
            let queue = app.world().resource::<RemeshQueue>();
            assert!(queue.dirty.contains_key(&a));
            assert!(!queue.dirty.contains_key(&b), "interior edit must not dirty the neighbor");
        }
        app.world_mut().resource_mut::<RemeshQueue>().dirty.clear();

        // Boundary edit on a's +X face: b's view of a changed → b dirties.
        app.world_mut().get_mut::<VoxelData>(a).unwrap().0
            [31 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        app.update();
        let queue = app.world().resource::<RemeshQueue>();
        assert!(queue.dirty.contains_key(&a));
        assert!(queue.dirty.contains_key(&b), "+X boundary edit must dirty the +X neighbor");
    }

    #[test]
    fn despawned_chunks_leave_the_grid() {
        let mut app = grid_app();
        let e = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[]));
        app.update();
        assert!(!app.world().resource::<ChunkGrid>().0.is_empty());
        app.world_mut().entity_mut(e).despawn();
        app.update();
        assert!(app.world().resource::<ChunkGrid>().0.is_empty());
    }

    use super::super::{apply_meshed_chunks, MeshingTask};
    use super::super::super::block_textures::BlockTextureLayers;
    use super::super::super::voxel_material;
    use super::super::binary::Face;
    use super::super::pack;
    use bevy::mesh::VertexAttributeValues;

    fn pipeline_app(budgets: MeshBudgets) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<ChunkGrid>()
            .init_resource::<RemeshQueue>()
            .init_resource::<BlockTextureLayers>()
            .insert_resource(budgets)
            .add_systems(
                Update,
                (mark_remesh_dirty, prune_chunk_grid, spawn_mesh_tasks, apply_meshed_chunks)
                    .chain(),
            );
        app
    }

    #[test]
    fn in_flight_budget_is_respected_and_nearest_meshes_first() {
        let mut app = pipeline_app(MeshBudgets { max_in_flight: 1, uploads_per_frame: 32 });
        // Camera at origin.
        app.world_mut().spawn((
            Camera3d::default(),
            GlobalTransform::from(Transform::from_translation(Vec3::ZERO)),
        ));
        let near = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        let far = spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(320.0, 0.0, 0.0), voxels_with(&[[5, 5, 5]]));
        app.update();
        // The near chunk claimed the single slot. `apply_meshed_chunks` chains
        // right after `spawn_mesh_tasks`, and the mesher is fast enough that a
        // tiny chunk's task can spawn, complete, AND apply within this one
        // update — so "claimed" shows as either an in-flight task or an
        // already-attached mesh, never as a queue entry.
        assert!(
            app.world().entity(near).contains::<MeshingTask>()
                || app.world().entity(near).contains::<Mesh3d>(),
            "nearest chunk claims the single task slot"
        );
        assert!(
            !app.world().resource::<RemeshQueue>().dirty.contains_key(&near),
            "nearest chunk left the queue"
        );
        assert!(
            !app.world().entity(far).contains::<MeshingTask>()
                && !app.world().entity(far).contains::<Mesh3d>(),
            "budget of 1 leaves the far chunk unmeshed"
        );
        assert!(app.world().resource::<RemeshQueue>().dirty.contains_key(&far));
    }

    /// Deadline-based wait: fixed-iteration polling flaked under full-suite
    /// load (the slow soak tests starve the shared AsyncComputeTaskPool, so a
    /// ~500ms budget failed runs that pass instantly in isolation). Returns
    /// true iff `cond` became true before the deadline.
    fn wait_until(app: &mut App, mut cond: impl FnMut(&mut App) -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            app.update();
            if cond(app) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        false
    }

    /// Waits until the remesh pipeline is fully settled: dirty queue drained
    /// and no `MeshingTask` in flight.
    fn wait_until_settled(app: &mut App) -> bool {
        wait_until(app, |app| {
            let queue_empty = app.world().resource::<RemeshQueue>().dirty.is_empty();
            let none_in_flight =
                app.world_mut().query::<&MeshingTask>().iter(app.world()).next().is_none();
            queue_empty && none_in_flight
        })
    }

    #[test]
    fn upload_budget_spreads_mesh_attach_over_frames() {
        let mut app = pipeline_app(MeshBudgets { max_in_flight: 64, uploads_per_frame: 1 });
        let a = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        let b = spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(32.0, 0.0, 0.0), voxels_with(&[[5, 5, 5]]));
        // Let tasks finish, then count Mesh3d arrivals per frame.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            app.update();
            let done = [a, b]
                .iter()
                .filter(|e| app.world().entity(**e).contains::<Mesh3d>())
                .count();
            if done == 1 {
                // Exactly one landed this frame — the cap held. Let the other land.
                assert!(
                    wait_until(&mut app, |app| {
                        app.world().entity(a).contains::<Mesh3d>()
                            && app.world().entity(b).contains::<Mesh3d>()
                    }),
                    "second mesh never attached"
                );
                return;
            }
            assert!(done < 2, "both meshes attached in one frame despite uploads_per_frame=1");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("no mesh ever attached");
    }

    #[test]
    fn meshing_uses_neighbor_data_from_the_grid() {
        // Two solid X-adjacent chunks: after both mesh, neither may have quads
        // on the shared plane. This is the end-to-end neighbor-culling test.
        let mut app = pipeline_app(MeshBudgets::default());
        let full: Vec<Voxel> = vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT];
        let a = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, full.clone());
        let b = spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(32.0, 0.0, 0.0), full);
        assert!(
            wait_until(&mut app, |app| {
                app.world().entity(a).contains::<Mesh3d>()
                    && app.world().entity(b).contains::<Mesh3d>()
            }),
            "both chunks must mesh"
        );
        // Chunk a spans x 0..32: with a solid +X neighbor its mesh must have
        // no vertex at x=32 with a +X face. (b meshed before a existed may
        // still carry its -X wall only if it meshed first without a — the
        // zone diff re-dirties it; settle until both are final.)
        assert!(wait_until_settled(&mut app), "pipeline must settle");
        let meshes = app.world().resource::<Assets<Mesh>>();
        let check = |e: Entity, plane_x: u32, expect_face: Face| {
            let handle = app.world().entity(e).get::<Mesh3d>().expect("meshed").0.clone();
            let mesh = meshes.get(&handle).unwrap();
            let Some(VertexAttributeValues::Uint32x2(packed)) =
                mesh.attribute(voxel_material::ATTRIBUTE_VOXEL_PACKED)
            else {
                panic!()
            };
            assert!(
                !packed.iter().any(|&pk| {
                    let v = pack::unpack(pk);
                    v.pos[0] == plane_x && v.face == expect_face
                }),
                "face on the shared plane survived neighbor culling"
            );
        };
        check(a, 32, Face::PosX);
        check(b, 0, Face::NegX);
    }

    #[test]
    fn keyed_entity_same_frame_remove_and_reinsert_stays_keyed_and_remeshes() {
        // Regression for finding 2: an entity that already carries a
        // `ChunkKey` must not fall out of remesh consideration when its
        // VoxelData is swapped within a single frame (the bridge's
        // remove-then-insert pattern).
        let mut app = pipeline_app(MeshBudgets::default());
        let e = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        assert!(
            wait_until(&mut app, |app| app.world().entity(e).contains::<Mesh3d>()),
            "initial mesh must land"
        );
        let key = app.world().entity(e).get::<ChunkKey>().expect("keyed after first mesh").0;
        let old_mesh = app.world().entity(e).get::<Mesh3d>().expect("meshed").0.clone();

        app.world_mut().entity_mut(e).remove::<VoxelData>();
        app.world_mut().entity_mut(e).insert(VoxelData(voxels_with(&[[6, 6, 6]])));
        app.update();

        assert!(app.world().entity(e).contains::<ChunkKey>(), "keeps ChunkKey across same-frame remove+reinsert");
        assert_eq!(
            app.world().resource::<ChunkGrid>().0.get(&key),
            Some(&e),
            "stays in the grid across same-frame remove+reinsert"
        );
        // The swap must trigger a remesh. The remesh may complete within the
        // same update (fast mesher + chained spawn→apply), so assert on the
        // guaranteed end-state — a NEW mesh handle lands — not on catching the
        // transient MeshingTask.
        assert!(
            wait_until(&mut app, |app| {
                app.world().entity(e).get::<Mesh3d>().is_some_and(|m| m.0 != old_mesh)
            }),
            "the voxel swap must remesh (a new mesh handle must land)"
        );
    }

    #[test]
    fn keyed_entity_cross_frame_remove_and_reinsert_reenters_the_grid() {
        // Direct regression for finding 2: once `prune_chunk_grid` evicts a
        // keyed entity (VoxelData gone for a whole frame), a later reinsert
        // of VoxelData must re-key it into the grid — the entity still
        // carries its old `ChunkKey`, so `mark_remesh_dirty`'s "insert only
        // when computing a fresh key" path never ran for it.
        let mut app = grid_app();
        let e = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[]));
        app.update();
        let key = app.world().entity(e).get::<ChunkKey>().expect("keyed on first update").0;
        assert_eq!(app.world().resource::<ChunkGrid>().0.get(&key), Some(&e));

        app.world_mut().entity_mut(e).remove::<VoxelData>();
        app.update();
        assert!(
            !app.world().resource::<ChunkGrid>().0.contains_key(&key),
            "evicted once VoxelData has been gone for a full frame"
        );
        assert!(app.world().entity(e).contains::<ChunkKey>(), "ChunkKey itself survives eviction");

        app.world_mut().entity_mut(e).insert(VoxelData(voxels_with(&[])));
        app.update();
        assert_eq!(
            app.world().resource::<ChunkGrid>().0.get(&key),
            Some(&e),
            "entity re-enters the grid under its old key after a cross-frame reinsert"
        );
    }

    #[test]
    fn despawning_a_chunk_redirties_its_neighbor() {
        // Direct regression for finding 1: two solid X-adjacent chunks mesh
        // and cull their shared plane; despawning one must re-dirty the
        // survivor so it re-grows the wall instead of leaving a hole.
        let mut app = pipeline_app(MeshBudgets::default());
        let full: Vec<Voxel> = vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT];
        let a = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, full.clone());
        let b = spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(32.0, 0.0, 0.0), full);
        assert!(
            wait_until(&mut app, |app| {
                app.world().entity(a).contains::<Mesh3d>() && app.world().entity(b).contains::<Mesh3d>()
            }),
            "both chunks must mesh"
        );
        // Settle: drain the dirty queue and let in-flight tasks finish so the
        // despawn below is the only thing left to react to.
        assert!(wait_until_settled(&mut app), "must be settled before the despawn");

        app.world_mut().entity_mut(a).despawn();

        // The re-dirty can be queued, claimed, or fully remeshed-and-applied
        // within a single update (fast mesher + chained systems), so assert on
        // the guaranteed end-state the fix exists for: b re-grows its shared
        // -X wall (a vertex at x=0 with a -X face) instead of leaving a hole.
        assert!(
            wait_until(&mut app, |app| {
                let Some(handle) = app.world().entity(b).get::<Mesh3d>().map(|m| m.0.clone()) else {
                    return false;
                };
                let meshes = app.world().resource::<Assets<Mesh>>();
                let Some(mesh) = meshes.get(&handle) else { return false };
                let Some(VertexAttributeValues::Uint32x2(packed)) =
                    mesh.attribute(voxel_material::ATTRIBUTE_VOXEL_PACKED)
                else {
                    return false;
                };
                packed.iter().any(|&pk| {
                    let v = pack::unpack(pk);
                    v.pos[0] == 0 && v.face == Face::NegX
                })
            }),
            "b must re-grow its shared-plane wall after its solid neighbor a is removed"
        );
    }
}
