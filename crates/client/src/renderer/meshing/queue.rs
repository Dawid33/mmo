//! Remesh scheduling: world-keyed chunk grid, boundary-zone change detection,
//! and the dirty queue drained by `spawn_mesh_tasks` (task 7). All state here
//! is render-derived — never rollback-hashed.

use std::collections::{BTreeMap, BTreeSet};

use bevy::camera::primitives::{Frustum, Sphere};
use bevy::prelude::*;
use bevy::tasks::AsyncComputeTaskPool;

use super::super::block_textures::BlockTextureLayers;
use super::super::bridge::VoxelData;
use super::neighbors::{gather_padded_rle, zone_index, zone_indices, zone_offsets};
use super::{mesh_chunk, MeshingTask};

/// World-space brick grid: key = world position / `BRICK_VOXELS`. Render-side
/// spatial index for neighbor lookups; rebuilt from entities, never persisted.
/// Its entries are render bricks (`bricks::BrickOf`), not sim chunks.
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

/// Floor on concurrently queued meshing tasks (the live cap scales with the
/// upload budget, see [`MeshBudgetPolicy`]). Each task owns a ~77 KB padded
/// snapshot; the cap bounds join-burst memory and lets priority reordering
/// act on everything not yet started. Tunable.
pub const MAX_IN_FLIGHT_MESH_TASKS: usize = 64;
/// Floor on finished meshes attached per frame, and the fixed value wherever
/// no [`MeshBudgetPolicy`] runs (tests). Tunable.
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

/// Target rate for attaching finished meshes, in chunks per second. The
/// per-frame cap is derived from it and the real frame delta
/// ([`per_frame_budget`]): ~50 per frame at 60 Hz, more when frames are long.
///
/// Why a rate: a fixed per-frame count throttles hardest exactly when frames
/// are slow. Measured 2026-09-19/20 at the production world size, the near
/// window drained at `MESH_UPLOADS_PER_FRAME x fps` — ~260 chunks/s at 8 fps,
/// ~1600/s at 45 fps — with the in-flight task cap pinned full the whole time,
/// i.e. the workers were never the limit. Tunable.
///
/// 2026-09-23: 3000 -> 6000 once `boundary_zones` moved off the main thread
/// (the 5x5 window's ~55k near chunks drained at exactly 3000/s, 18+ s).
pub const MESH_UPLOADS_PER_SECOND: f32 = 6000.0;
/// Ceiling on the derived per-frame upload cap: twice a 60 Hz frame's worth.
/// A budget proportional to the frame delta is a feedback loop — a long frame
/// earns more work, which makes the next frame longer — so the ceiling is what
/// keeps it bounded. The first version allowed 256 (and 512 snapshot inserts)
/// and the first seconds of a join ran at 37-49 ms per frame with 31 ms
/// `mark_remesh_dirty` stalls (2026-09-20 capture).
pub const MAX_MESH_UPLOADS_PER_FRAME: usize = 100;
/// In-flight tasks allowed per upload slot: a finished task holds its slot
/// until it is attached, so the lane needs headroom beyond one frame's worth.
pub const IN_FLIGHT_PER_UPLOAD: usize = 2;

/// A per-second work rate as a per-frame count for a frame that took `dt`,
/// clamped to `min..=max`. `min` keeps high refresh rates from starving the
/// work; `max` bounds what a single long frame may take on.
pub fn per_frame_budget(per_second: f32, dt: std::time::Duration, min: usize, max: usize) -> usize {
    let scaled = (per_second * dt.as_secs_f32()).round();
    // NaN / negative / huge all land inside the clamp.
    (scaled.max(0.0) as usize).clamp(min, max.max(min))
}

/// The rate policy behind [`MeshBudgets`]. Production inserts it and runs
/// [`scale_mesh_budgets`]; harnesses that pin `MeshBudgets` by hand simply
/// don't, and keep their fixed per-frame numbers.
#[derive(Resource, Clone, Copy, Debug)]
pub struct MeshBudgetPolicy {
    pub uploads_per_second: f32,
    pub min_uploads_per_frame: usize,
    pub max_uploads_per_frame: usize,
    pub min_in_flight: usize,
}

impl Default for MeshBudgetPolicy {
    fn default() -> Self {
        Self {
            uploads_per_second: MESH_UPLOADS_PER_SECOND,
            min_uploads_per_frame: MESH_UPLOADS_PER_FRAME,
            max_uploads_per_frame: MAX_MESH_UPLOADS_PER_FRAME,
            min_in_flight: MAX_IN_FLIGHT_MESH_TASKS,
        }
    }
}

/// Re-derive this frame's [`MeshBudgets`] from the policy and the real frame
/// delta. First in the meshing chain, so every lane sees the same numbers.
pub fn scale_mesh_budgets(
    time: Res<Time<Real>>,
    policy: Res<MeshBudgetPolicy>,
    mut budgets: ResMut<MeshBudgets>,
) {
    let uploads = per_frame_budget(
        policy.uploads_per_second,
        time.delta(),
        policy.min_uploads_per_frame,
        policy.max_uploads_per_frame,
    );
    let in_flight = policy.min_in_flight.max(uploads * IN_FLIGHT_PER_UPLOAD);
    if budgets.uploads_per_frame != uploads || budgets.max_in_flight != in_flight {
        budgets.uploads_per_frame = uploads;
        budgets.max_in_flight = in_flight;
    }
}

/// Per-frame count of near mesh uploads (`Mesh3d`/quad insertions). Far raster
/// LOD uses its own equally bounded `lod::FarFrameUploads` counter, so neither
/// lane can starve the other. Reset here by `mark_remesh_dirty`, the first
/// system in the chained Update pipeline.
#[derive(Resource, Default)]
pub struct FrameUploads(pub usize);

/// Sort rank: out-of-frustum chunks after in-frustum, then by squared
/// distance to the camera. No camera (headless/startup) ranks everything 0.
fn chunk_rank(key: [i32; 3], cam: Option<(&GlobalTransform, Option<&Frustum>)>) -> u64 {
    let Some((tf, frustum)) = cam else { return 0 };
    let (edge, half) = (game::BRICK_VOXELS as f32, game::BRICK_VOXELS as f32 / 2.0);
    let center = Vec3::new(
        key[0] as f32 * edge + half,
        key[1] as f32 * edge + half,
        key[2] as f32 * edge + half,
    );
    let dist2 = tf.translation().distance_squared(center) as u64;
    let out = frustum
        .map(|f| {
            !f.intersects_sphere(
                &Sphere {
                    center: center.into(),
                    radius: half * 3f32.sqrt(),
                },
                false,
            )
        })
        .unwrap_or(false);
    ((out as u64) << 62) | dist2.min((1 << 62) - 1)
}

/// Drain the dirty queue into meshing tasks, nearest-first, up to the
/// in-flight budget. Re-inserting `MeshingTask` supersedes (cancels) any
/// prior in-flight task for that entity — same rule as the old pipeline.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn spawn_mesh_tasks(
    mut commands: Commands,
    mut queue: ResMut<RemeshQueue>,
    budgets: Res<MeshBudgets>,
    grid: Res<ChunkGrid>,
    chunks: Query<&VoxelData>,
    in_flight: Query<(), With<MeshingTask>>,
    camera: Query<(&GlobalTransform, Option<&Frustum>), With<Camera3d>>,
    layers: Res<BlockTextureLayers>,
    render_config: Option<Res<super::super::config::TerrainRenderConfig>>,
) {
    if queue.dirty.is_empty() {
        return;
    }
    let pull = render_config
        .as_deref()
        .is_none_or(|config| config.near.uses_quad_records());
    // Entities that lost VoxelData while queued are stale — drop them.
    queue.dirty.retain(|e, _| chunks.contains(*e));
    let capacity = budgets
        .max_in_flight
        .saturating_sub(in_flight.iter().count());
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
        // Clone only compact RLE components while ECS access is available.
        // Expansion into the owned padded snapshot happens inside the async
        // task, keeping join/remesh preparation off the main thread.
        let center = voxels.clone();
        let mut owned_neighbors: Vec<([i32; 3], VoxelData)> = Vec::new();
        if let Some(k) = key {
            for off in zone_offsets() {
                let nk = [k[0] + off[0], k[1] + off[1], k[2] + off[2]];
                if let Some(&ne) = grid.0.get(&nk) {
                    if let Ok(nv) = chunks.get(ne) {
                        owned_neighbors.push((off, nv.clone()));
                    }
                }
            }
        }
        let layers = layers.0.clone();
        let task = pool.spawn(async move {
            if is_buried(&center, &owned_neighbors) {
                return buried_mesh_output();
            }
            let neighbors = owned_neighbors
                .iter()
                .map(|(off, data)| (*off, data))
                .collect();
            let padded = gather_padded_rle(&center, &neighbors);
            mesh_chunk(&padded, &BlockTextureLayers(layers), pull)
        });
        commands.entity(e).insert(MeshingTask(task));
    }
}

/// A chunk with no air whose 6 face neighbours all exist and present no air
/// on the shared plane: the mesher would emit no face (faces only border air)
/// and find no air to connect, so `mesh_chunk` is known without running it.
/// ~30% of a join's chunks (2026-09-22 findings); skipping the padded gather,
/// mesher and connectivity flood saves ~160 µs of worker time each. Edges and
/// corners are irrelevant: they only feed AO, which needs a face. Checked in
/// the task, not the main thread: the plane scans are ~6k voxel visits.
pub fn is_buried(center: &VoxelData, neighbors: &[([i32; 3], VoxelData)]) -> bool {
    is_buried_by(center, |off| neighbors.iter().find(|(o, _)| *o == off).map(|(_, v)| v))
}

/// [`is_buried`] over any neighbour lookup (`off` is the face offset; `None`
/// is a missing neighbour, which never buries).
pub fn is_buried_by<'a>(center: &VoxelData, neighbor: impl Fn([i32; 3]) -> Option<&'a VoxelData>) -> bool {
    if !center.has_no_air() {
        return false;
    }
    let faces = [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]];
    faces.into_iter().all(|off| {
        neighbor(off).is_some_and(|neighbor| {
            if neighbor.has_no_air() {
                return true;
            }
            let mut solid = true;
            neighbor.for_each_index_sorted(zone_indices([-off[0], -off[1], -off[2]]), |_, voxel| {
                solid &= !voxel.is_air();
            });
            solid
        })
    })
}

/// What `mesh_chunk` returns for a buried chunk, in either render mode.
pub fn buried_mesh_output() -> super::MeshOutput {
    super::MeshOutput { mesh: None, records: None, connectivity: 0, quad_count: 0 }
}

/// Hash the 26 boundary zones of a 32³ chunk. Zones overlap on purpose: a
/// corner voxel contributes to 3 face + 3 edge + 1 corner zone.
pub fn boundary_zones(voxels: &VoxelData) -> [u32; 26] {
    voxels
        .cached_boundary_zones()
        .unwrap_or_else(|| compute_boundary_zones(voxels))
}

/// The uncached hashing behind `boundary_zones`.
pub fn compute_boundary_zones(voxels: &VoxelData) -> [u32; 26] {
    let mut zones = [0u32; 26];
    for (i, off) in zone_offsets().enumerate() {
        let mut h = crc32fast::Hasher::new();
        voxels.for_each_index_sorted(zone_indices(off), |_, voxel| {
            h.update(&voxel.block.0.to_le_bytes());
        });
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
    mut uploads: ResMut<FrameUploads>,
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
    uploads.0 = 0;
    // Unbudgeted: every chunk changed this frame is fingerprinted here.
    let mut stall = game::perf::StallGuard::new("mark_remesh_dirty");
    for (e, voxels, tf, parent, key, mut zones) in &mut changed {
        stall.add_work(1);
        // Resolve (or compute) the grid key. Bare test entities (no parented
        // transform) stay unkeyed and skip all neighbor logic.
        let key: Option<[i32; 3]> = key.map(|k| k.0).or_else(|| {
            let (tf, parent) = (tf?, parent?);
            let root = roots.get(parent.parent()).ok()?;
            let world = (root.translation + tf.translation) / game::BRICK_VOXELS as f32;
            let k = [world.x.round() as i32, world.y.round() as i32, world.z.round() as i32];
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

        let new_zones = boundary_zones(voxels);
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
    let dead: BTreeSet<Entity> = removed
        .read()
        .filter(|e| !still_alive.contains(*e))
        .collect();
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
pub(crate) mod tests {
    use super::*;

    /// `is_buried` must only claim chunks the full mesher agrees are empty:
    /// no quads, no connectivity, in both render modes.
    #[test]
    fn buried_shortcut_matches_the_full_mesher() {
        use game::{BlockId, Voxel, BRICK_VOXEL_COUNT};
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move |modulus: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % modulus
        };
        let idx = |x: usize, y: usize, z: usize| x + 32 * y + 32 * 32 * z;
        // 0: all stone, 1: stone + water (both non-air), 2: stone with one
        // air voxel at a random spot (interior, face, edge or corner).
        let chunk = |next: &mut dyn FnMut(u64) -> u64, kind: u64| -> VoxelData {
            let mut v = vec![Voxel::new(BlockId(1)); BRICK_VOXEL_COUNT];
            match kind {
                1 => {
                    for i in 0..BRICK_VOXEL_COUNT / 2 {
                        v[i] = Voxel::new(BlockId(7));
                    }
                }
                2 => {
                    let pick = |next: &mut dyn FnMut(u64) -> u64| [0, 31, 1 + next(30) as usize][next(3) as usize];
                    let (x, y, z) = (pick(next), pick(next), pick(next));
                    v[idx(x, y, z)] = Voxel::default();
                }
                _ => {}
            }
            VoxelData::new(v)
        };
        let layers = BlockTextureLayers::default();
        let mut buried_cases = 0;
        for round in 0..400 {
            let center_kind = [0, 0, 1, 2][next(4) as usize];
            let center = chunk(&mut next, center_kind);
            let mut neighbors: Vec<([i32; 3], VoxelData)> = Vec::new();
            for off in zone_offsets() {
                // Mostly solid; sometimes missing or holed (anywhere).
                match next(8) {
                    0 => {}
                    1 => neighbors.push((off, chunk(&mut next, 2))),
                    k => neighbors.push((off, chunk(&mut next, k % 2))),
                }
            }
            let buried = is_buried(&center, &neighbors);
            let refs: BTreeMap<[i32; 3], &VoxelData> = neighbors.iter().map(|(o, v)| (*o, v)).collect();
            let padded = gather_padded_rle(&center, &refs);
            for pull in [true, false] {
                let full = mesh_chunk(&padded, &layers, pull);
                if buried {
                    assert_eq!(full.quad_count, 0, "round {round} pull={pull}: buried chunk has faces");
                    assert_eq!(full.connectivity, 0, "round {round}: buried chunk connects");
                    assert!(full.records.is_none() && full.mesh.is_none(), "round {round}");
                }
            }
            buried_cases += usize::from(buried);
        }
        assert!(buried_cases >= 20, "too few buried cases to mean anything: {buried_cases}");
    }

    #[test]
    fn per_frame_budget_scales_with_frame_time_inside_its_clamp() {
        use std::time::Duration;
        let budget = |ms: u64| per_frame_budget(3000.0, Duration::from_millis(ms), 32, 256);
        assert_eq!(budget(16), 48, "~60 Hz");
        assert_eq!(budget(25), 75, "40 fps: more per frame, same per second");
        assert_eq!(budget(5), 32, "high refresh rates hit the floor");
        assert_eq!(budget(0), 32, "first frame / zero delta");
        assert_eq!(budget(125), 256, "an 8 fps frame hits the ceiling, not 375");
        assert_eq!(budget(10_000), 256);
        // Throughput no longer collapses with the frame rate (it used to be
        // 32 x fps): every delta between the clamps yields ~3000/s.
        for ms in [12u64, 16, 25, 33, 50, 80] {
            let per_second = budget(ms) as f32 * 1000.0 / ms as f32;
            assert!((2900.0..=3100.0).contains(&per_second), "{ms} ms -> {per_second}/s");
        }
        assert_eq!(per_frame_budget(f32::NAN, Duration::from_millis(16), 8, 64), 8);
        assert_eq!(per_frame_budget(3000.0, Duration::from_millis(16), 64, 8), 64, "min wins over a lower max");
    }

    #[test]
    fn scale_mesh_budgets_tracks_the_policy_and_keeps_in_flight_headroom() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<MeshBudgets>()
            .insert_resource(MeshBudgetPolicy {
                uploads_per_second: 10_000.0,
                min_uploads_per_frame: 4,
                max_uploads_per_frame: 100,
                min_in_flight: 16,
            })
            .add_systems(Update, scale_mesh_budgets);
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(20));
        app.update(); // ~20 ms frame at 10k/s => ~200, clamped to 100
        let budgets = app.world().resource::<MeshBudgets>();
        assert_eq!(budgets.uploads_per_frame, 100);
        assert_eq!(budgets.max_in_flight, 100 * IN_FLIGHT_PER_UPLOAD);
    }
    use game::{BlockId, Voxel, BRICK_VOXEL_COUNT};

    pub(crate) fn voxels_with(cells: &[[usize; 3]]) -> Vec<Voxel> {
        let mut v = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        for [x, y, z] in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(1));
        }
        v
    }

    #[test]
    fn boundary_zone_reacts_only_to_its_zone() {
        let a = boundary_zones(&VoxelData::new(voxels_with(&[[5, 5, 5]])));
        let b = boundary_zones(&VoxelData::new(voxels_with(&[[5, 5, 6]])));
        assert_eq!(a, b, "interior-only changes leave every zone hash equal");

        let c = boundary_zones(&VoxelData::new(voxels_with(&[[31, 5, 5]])));
        let face_px = zone_index([1, 0, 0]);
        assert_ne!(a[face_px], c[face_px], "+X face zone changed");
        let face_nx = zone_index([-1, 0, 0]);
        assert_eq!(a[face_nx], c[face_nx], "-X face zone did not");

        let d = boundary_zones(&VoxelData::new(voxels_with(&[[31, 31, 31]])));
        assert_ne!(
            a[zone_index([1, 1, 1])],
            d[zone_index([1, 1, 1])],
            "corner zone changed"
        );
        assert_ne!(
            a[zone_index([1, 1, 0])],
            d[zone_index([1, 1, 0])],
            "edge zone changed"
        );
        assert_ne!(
            a[zone_index([1, 0, 0])],
            d[zone_index([1, 0, 0])],
            "face zone changed"
        );
        // The cache (warmed off-thread by the snapshot-voxel task) must agree
        // with the on-demand hashing, and an edit must drop it.
        let warm = VoxelData::new(voxels_with(&[[31, 31, 31]])).with_boundary_zones();
        assert_eq!(warm.cached_boundary_zones(), Some(d));
        assert_eq!(boundary_zones(&warm), d);
        let mut edited = warm.clone();
        edited.edit(|v| v[0] = Voxel::new(game::BlockId(1)));
        assert_eq!(edited.cached_boundary_zones(), None, "edit re-encodes without the cache");
    }

    fn grid_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ChunkGrid>()
            .init_resource::<RemeshQueue>()
            .init_resource::<FrameUploads>()
            .add_systems(Update, (mark_remesh_dirty, prune_chunk_grid));
        app
    }

    /// Region-root + chunk child at region-local chunk coords, like the bridge builds.
    pub(crate) fn spawn_chunk(
        app: &mut App,
        root_at: Vec3,
        local: Vec3,
        voxels: Vec<Voxel>,
    ) -> Entity {
        let root = app
            .world_mut()
            .spawn((Transform::from_translation(root_at), Visibility::default()))
            .id();
        app.world_mut()
            .spawn((
                Transform::from_translation(local),
                Visibility::default(),
                VoxelData::new(voxels),
                ChildOf(root),
            ))
            .id()
    }

    #[test]
    fn chunks_key_into_the_grid_by_world_position() {
        let mut app = grid_app();
        let e = spawn_chunk(
            &mut app,
            Vec3::new(256.0, 0.0, -512.0),
            Vec3::new(64.0, 32.0, 0.0),
            voxels_with(&[]),
        );
        app.update();
        let grid = app.world().resource::<ChunkGrid>();
        assert_eq!(
            grid.0.get(&[10, 1, -16]),
            Some(&e),
            "(256+64)/32, 32/32, -512/32"
        );
        assert!(app.world().resource::<RemeshQueue>().dirty.contains_key(&e));
    }

    #[test]
    fn boundary_change_dirties_only_affected_neighbors() {
        let mut app = grid_app();
        // Two X-adjacent chunks under one root at origin.
        let a = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        let b = spawn_chunk(
            &mut app,
            Vec3::ZERO,
            Vec3::new(32.0, 0.0, 0.0),
            voxels_with(&[]),
        );
        app.update();
        // Drain the initial dirtiness.
        app.world_mut().resource_mut::<RemeshQueue>().dirty.clear();

        // Interior edit in `a`: nobody's boundary view changes except a itself.
        app.world_mut()
            .get_mut::<VoxelData>(a)
            .unwrap()
            .edit(|v| v[6 + 32 * 6 + 32 * 32 * 6] = Voxel::new(BlockId(1)));
        app.update();
        {
            let queue = app.world().resource::<RemeshQueue>();
            assert!(queue.dirty.contains_key(&a));
            assert!(
                !queue.dirty.contains_key(&b),
                "interior edit must not dirty the neighbor"
            );
        }
        app.world_mut().resource_mut::<RemeshQueue>().dirty.clear();

        // Boundary edit on a's +X face: b's view of a changed → b dirties.
        app.world_mut()
            .get_mut::<VoxelData>(a)
            .unwrap()
            .edit(|v| v[31 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1)));
        app.update();
        let queue = app.world().resource::<RemeshQueue>();
        assert!(queue.dirty.contains_key(&a));
        assert!(
            queue.dirty.contains_key(&b),
            "+X boundary edit must dirty the +X neighbor"
        );
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

    use super::super::super::block_textures::BlockTextureLayers;
    use super::super::super::voxel_material;
    use super::super::binary::Face;
    use super::super::pack;
    use super::super::{apply_meshed_chunks, MeshingTask};
    use bevy::mesh::VertexAttributeValues;

    /// Full pipeline as production registers it in `renderer/mod.rs`,
    /// including the LOD mesh path chained after `apply_meshed_chunks` and
    /// the cave-culling pass via `cull_plugin` (PostUpdate).
    pub(crate) fn pipeline_app(budgets: MeshBudgets) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<ChunkGrid>()
            .init_resource::<RemeshQueue>()
            .init_resource::<FrameUploads>()
            .init_resource::<super::super::lod::LodBuildQueue>()
            .init_resource::<super::super::lod::FarFrameUploads>()
            .init_resource::<BlockTextureLayers>()
            .init_resource::<super::super::visibility::CullingConfig>()
            // These pipeline tests introspect `Mesh3d` / `ATTRIBUTE_VOXEL_PACKED`,
            // so pin the legacy per-chunk backend.
            .insert_resource(super::super::super::config::TerrainRenderConfig {
                near: super::super::super::config::NearRenderBackend::PerChunk,
                far: super::super::super::config::FarRenderBackend::Raster,
            })
            .insert_resource(budgets)
            .add_systems(
                Update,
                (
                    mark_remesh_dirty,
                    prune_chunk_grid,
                    spawn_mesh_tasks,
                    apply_meshed_chunks,
                    super::super::lod::spawn_lod_mesh_tasks,
                    super::super::lod::apply_lod_meshes,
                )
                    .chain(),
            )
            .add_plugins(super::super::visibility::cull_plugin);
        app
    }

    #[test]
    fn in_flight_budget_is_respected_and_nearest_meshes_first() {
        let mut app = pipeline_app(MeshBudgets {
            max_in_flight: 1,
            uploads_per_frame: 32,
        });
        // Camera at origin.
        app.world_mut().spawn((
            Camera3d::default(),
            GlobalTransform::from(Transform::from_translation(Vec3::ZERO)),
        ));
        let near = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        let far = spawn_chunk(
            &mut app,
            Vec3::ZERO,
            Vec3::new(320.0, 0.0, 0.0),
            voxels_with(&[[5, 5, 5]]),
        );
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
            !app.world()
                .resource::<RemeshQueue>()
                .dirty
                .contains_key(&near),
            "nearest chunk left the queue"
        );
        assert!(
            !app.world().entity(far).contains::<MeshingTask>()
                && !app.world().entity(far).contains::<Mesh3d>(),
            "budget of 1 leaves the far chunk unmeshed"
        );
        assert!(app
            .world()
            .resource::<RemeshQueue>()
            .dirty
            .contains_key(&far));
    }

    /// Deadline-based wait: fixed-iteration polling flaked under full-suite
    /// load (the slow soak tests starve the shared AsyncComputeTaskPool, so a
    /// ~500ms budget failed runs that pass instantly in isolation). Returns
    /// true iff `cond` became true before the deadline.
    pub(crate) fn wait_until(app: &mut App, mut cond: impl FnMut(&mut App) -> bool) -> bool {
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
    pub(crate) fn wait_until_settled(app: &mut App) -> bool {
        wait_until(app, |app| {
            let queue_empty = app.world().resource::<RemeshQueue>().dirty.is_empty();
            let none_in_flight = app
                .world_mut()
                .query::<&MeshingTask>()
                .iter(app.world())
                .next()
                .is_none();
            queue_empty && none_in_flight
        })
    }

    #[test]
    fn upload_budget_spreads_mesh_attach_over_frames() {
        let mut app = pipeline_app(MeshBudgets {
            max_in_flight: 64,
            uploads_per_frame: 1,
        });
        let a = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        let b = spawn_chunk(
            &mut app,
            Vec3::ZERO,
            Vec3::new(32.0, 0.0, 0.0),
            voxels_with(&[[5, 5, 5]]),
        );
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
            assert!(
                done < 2,
                "both meshes attached in one frame despite uploads_per_frame=1"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("no mesh ever attached");
    }

    #[test]
    fn meshing_uses_neighbor_data_from_the_grid() {
        // Two solid X-adjacent chunks: after both mesh, neither may have quads
        // on the shared plane. This is the end-to-end neighbor-culling test.
        let mut app = pipeline_app(MeshBudgets::default());
        let full: Vec<Voxel> = vec![Voxel::new(BlockId(1)); BRICK_VOXEL_COUNT];
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
            let handle = app
                .world()
                .entity(e)
                .get::<Mesh3d>()
                .expect("meshed")
                .0
                .clone();
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
        let key = app
            .world()
            .entity(e)
            .get::<ChunkKey>()
            .expect("keyed after first mesh")
            .0;
        let old_mesh = app
            .world()
            .entity(e)
            .get::<Mesh3d>()
            .expect("meshed")
            .0
            .clone();

        app.world_mut().entity_mut(e).remove::<VoxelData>();
        app.world_mut()
            .entity_mut(e)
            .insert(VoxelData::new(voxels_with(&[[6, 6, 6]])));
        app.update();

        assert!(
            app.world().entity(e).contains::<ChunkKey>(),
            "keeps ChunkKey across same-frame remove+reinsert"
        );
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
                app.world()
                    .entity(e)
                    .get::<Mesh3d>()
                    .is_some_and(|m| m.0 != old_mesh)
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
        let key = app
            .world()
            .entity(e)
            .get::<ChunkKey>()
            .expect("keyed on first update")
            .0;
        assert_eq!(app.world().resource::<ChunkGrid>().0.get(&key), Some(&e));

        app.world_mut().entity_mut(e).remove::<VoxelData>();
        app.update();
        assert!(
            !app.world().resource::<ChunkGrid>().0.contains_key(&key),
            "evicted once VoxelData has been gone for a full frame"
        );
        assert!(
            app.world().entity(e).contains::<ChunkKey>(),
            "ChunkKey itself survives eviction"
        );

        app.world_mut()
            .entity_mut(e)
            .insert(VoxelData::new(voxels_with(&[])));
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
        let full: Vec<Voxel> = vec![Voxel::new(BlockId(1)); BRICK_VOXEL_COUNT];
        let a = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, full.clone());
        let b = spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(32.0, 0.0, 0.0), full);
        assert!(
            wait_until(&mut app, |app| {
                app.world().entity(a).contains::<Mesh3d>()
                    && app.world().entity(b).contains::<Mesh3d>()
            }),
            "both chunks must mesh"
        );
        // Settle: drain the dirty queue and let in-flight tasks finish so the
        // despawn below is the only thing left to react to.
        assert!(
            wait_until_settled(&mut app),
            "must be settled before the despawn"
        );

        app.world_mut().entity_mut(a).despawn();

        // The re-dirty can be queued, claimed, or fully remeshed-and-applied
        // within a single update (fast mesher + chained systems), so assert on
        // the guaranteed end-state the fix exists for: b re-grows its shared
        // -X wall (a vertex at x=0 with a -X face) instead of leaving a hole.
        assert!(
            wait_until(&mut app, |app| {
                let Some(handle) = app.world().entity(b).get::<Mesh3d>().map(|m| m.0.clone())
                else {
                    return false;
                };
                let meshes = app.world().resource::<Assets<Mesh>>();
                let Some(mesh) = meshes.get(&handle) else {
                    return false;
                };
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
