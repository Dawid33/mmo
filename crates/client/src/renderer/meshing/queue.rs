//! Remesh scheduling: world-keyed chunk grid, boundary-zone change detection,
//! and the dirty queue drained by `spawn_mesh_tasks` (task 7). All state here
//! is render-derived — never rollback-hashed.

use std::collections::BTreeMap;

use bevy::prelude::*;
use game::{Voxel, CHUNK_VOXEL_COUNT};

use super::super::bridge::VoxelData;
use super::neighbors::{zone_index, zone_offsets};

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
            grid.0.insert(k, e);
            commands.entity(e).insert(ChunkKey(k));
            Some(k)
        });
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
/// frame's removal log, so same-frame remove+reinsert survives).
pub fn prune_chunk_grid(
    mut grid: ResMut<ChunkGrid>,
    mut queue: ResMut<RemeshQueue>,
    mut removed: RemovedComponents<VoxelData>,
    still_alive: Query<(), With<VoxelData>>,
) {
    let dead: Vec<Entity> = removed.read().filter(|e| !still_alive.contains(*e)).collect();
    if dead.is_empty() {
        return;
    }
    grid.0.retain(|_, e| !dead.contains(e));
    for e in dead {
        queue.dirty.remove(&e);
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
}
