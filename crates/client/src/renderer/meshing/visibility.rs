//! Cave-culling visibility graph (Tommo/MCPE "Advanced Cave Culling"):
//! per-chunk face-pair connectivity computed by flood fill over the chunk's
//! own 32³ non-solid space, and the per-frame camera BFS that hides chunks
//! no sight line can reach. All state here is render-derived — recomputed on
//! remesh, never rollback-hashed.
//! Reference: https://tomcc.github.io/2014/08/31/visibility-1.html

use std::collections::{HashSet, VecDeque};

use bevy::camera::primitives::{Frustum, Sphere};
use bevy::prelude::*;

use super::binary::PaddedVoxels;
use super::queue::{ChunkGrid, ChunkKey};

/// All 15 face pairs connected — the mask of an all-air (or absent) chunk.
pub const ALL_CONNECTED: u16 = 0x7FFF;

/// The chunk's face-pair connectivity, attached next to `Mesh3d` by
/// `apply_meshed_chunks`. Derived render state — recomputed on every remesh.
#[derive(bevy::prelude::Component, Copy, Clone)]
pub struct ChunkConnectivity(pub u16);

/// Index of the unordered face pair (a, b), a < b, faces in `Face::index()`
/// order. Bijection onto 0..15 (test-pinned).
pub fn pair_index(a: u32, b: u32) -> u32 {
    debug_assert!(a < b && b < 6);
    // Formula rewritten to avoid underflow: a * (11 - a) / 2 + (b - a - 1)
    // (equivalent to a * 5 - a * (a - 1) / 2 + (b - a - 1))
    a * (11 - a) / 2 + (b - a - 1)
}

/// Bit for the unordered pair (a, b) in either order.
pub fn pair_bit(a: u32, b: u32) -> u16 {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    1 << pair_index(lo, hi)
}

/// Which of a cell's chunk-local coords sit on which face, in Face::index()
/// order: +X = x == 31, -X = x == 0, +Y = y == 31, -Y = y == 0, +Z/-Z same.
fn touched_faces(x: usize, y: usize, z: usize) -> u8 {
    let mut f = 0u8;
    if x == 31 { f |= 1 << 0; }
    if x == 0  { f |= 1 << 1; }
    if y == 31 { f |= 1 << 2; }
    if y == 0  { f |= 1 << 3; }
    if z == 31 { f |= 1 << 4; }
    if z == 0  { f |= 1 << 5; }
    f
}

/// Flood-fill the chunk's own 32³ non-solid space (padded interior 1..=32;
/// the 0/33 neighbor shell is deliberately excluded) and record, for each
/// connected air component, which chunk faces it touches; every pair of
/// touched faces is connected. O(32³) worst case, a few hundred µs — runs
/// inside the async mesh task, never on the main thread.
pub fn face_connectivity(p: &PaddedVoxels) -> u16 {
    const N: usize = 32;
    let idx = |x: usize, y: usize, z: usize| x + N * (y + N * z);
    let mut visited = vec![false; N * N * N];
    let mut mask = 0u16;
    let mut stack: Vec<(usize, usize, usize)> = Vec::new();

    for sz in 0..N {
        for sy in 0..N {
            for sx in 0..N {
                if visited[idx(sx, sy, sz)] || p.is_solid(sx + 1, sy + 1, sz + 1) {
                    continue;
                }
                // New air component: flood it, accumulate touched faces.
                let mut faces = 0u8;
                visited[idx(sx, sy, sz)] = true;
                stack.push((sx, sy, sz));
                while let Some((x, y, z)) = stack.pop() {
                    faces |= touched_faces(x, y, z);
                    let mut push = |nx: usize, ny: usize, nz: usize| {
                        if !visited[idx(nx, ny, nz)] && !p.is_solid(nx + 1, ny + 1, nz + 1) {
                            visited[idx(nx, ny, nz)] = true;
                            stack.push((nx, ny, nz));
                        }
                    };
                    if x + 1 < N { push(x + 1, y, z); }
                    if x > 0     { push(x - 1, y, z); }
                    if y + 1 < N { push(x, y + 1, z); }
                    if y > 0     { push(x, y - 1, z); }
                    if z + 1 < N { push(x, y, z + 1); }
                    if z > 0     { push(x, y, z - 1); }
                }
                // Union all pairs among the touched faces.
                for a in 0..6u32 {
                    if faces >> a & 1 == 0 { continue; }
                    for b in (a + 1)..6u32 {
                        if faces >> b & 1 == 1 {
                            mask |= pair_bit(a, b);
                        }
                    }
                }
                if mask == ALL_CONNECTED {
                    return mask; // can't grow further
                }
            }
        }
    }
    mask
}

/// Runtime switch for the camera-BFS cave culling (F4). Off ⇒ every chunk is
/// restored to `Inherited` and stays that way.
#[derive(Resource)]
pub struct CullingConfig {
    pub graph_enabled: bool,
}

impl Default for CullingConfig {
    fn default() -> Self {
        Self { graph_enabled: true }
    }
}

/// Hard bound on the BFS expansion (Chebyshev radius in chunk cells around
/// the camera cell). Cells outside the expansion domain are never reached and
/// their chunks end Hidden, so this must overshoot the largest loaded view
/// window — today that's the near 3×3 region window alone, ≈24 chunks across
/// (`ChunkGrid`, keyed by `ChunkKey`, holds only near-window chunks). The far
/// ring and the worldgen rings beyond it (Phase 4) render `LodChunk` entities,
/// which carry no `ChunkKey` and so are keyless to this BFS: `cull_chunks`
/// queries `With<ChunkKey>` and never reaches or hides them, at any view
/// distance. 64 overshoots the near window by more than 2×.
pub const CULL_BFS_MAX_CHEBYSHEV: i32 = 64;

/// Entry-face sentinel for the BFS start cell: no entry face, all exits
/// allowed regardless of the cell's connectivity mask.
const NO_ENTRY: u8 = 6;

/// Unit steps in `Face::index()` order (PosX, NegX, PosY, NegY, PosZ, NegZ) —
/// the locked order in which `opposite(d) = d ^ 1`.
const DIR_STEPS: [[i32; 3]; 6] =
    [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]];

/// Per-frame camera BFS over the chunk grid (Tommo cave culling): flood
/// outward from the camera cell; a step from a cell with a connectivity mask
/// is allowed only if the mask connects the entry face to the exit face, a
/// direction whose opposite was already used on the path is banned
/// (no-backtrack), and neighbors outside the camera frustum are rejected.
/// Chunks whose cell the flood never reaches are `Hidden`.
///
/// Guard rails (under-visibility means holes in the world): grid-absent cells
/// and chunks without a `ChunkConnectivity` yet traverse as fully-open air; a
/// missing `Frustum` (headless) counts as everything-inside; the camera cell
/// is clamped into the grid's inflated bounding box when outside it (flying
/// above the band still sees down into it).
pub fn cull_chunks(
    config: Res<CullingConfig>,
    grid: Res<ChunkGrid>,
    cameras: Query<(&GlobalTransform, Option<&Frustum>, &Camera), With<Camera3d>>,
    masks: Query<&ChunkConnectivity>,
    mut chunks: Query<&mut Visibility, With<ChunkKey>>,
) {
    if !config.graph_enabled {
        for mut vis in &mut chunks {
            if *vis != Visibility::Inherited {
                *vis = Visibility::Inherited;
            }
        }
        return;
    }
    // No active camera: leave visibility unchanged rather than flashing the
    // world hidden (startup, camera handoff mid-flight).
    let Some((camera_tf, frustum, _)) = cameras.iter().find(|(_, _, cam)| cam.is_active) else {
        return;
    };
    if grid.0.is_empty() {
        return;
    }

    // Expansion domain: the grid keys' bounding box inflated by 1 (so sight
    // lines may route through the open shell just outside the loaded band),
    // intersected with the Chebyshev ball around the camera cell. The camera
    // cell is clamped into the box first, which also keeps the ball centered
    // on the effective start cell.
    let mut lo = [i32::MAX; 3];
    let mut hi = [i32::MIN; 3];
    for key in grid.0.keys() {
        for axis in 0..3 {
            lo[axis] = lo[axis].min(key[axis]);
            hi[axis] = hi[axis].max(key[axis]);
        }
    }
    for axis in 0..3 {
        lo[axis] -= 1;
        hi[axis] += 1;
    }
    let translation = camera_tf.translation();
    let mut camera_cell = [
        (translation.x / 32.0).floor() as i32,
        (translation.y / 32.0).floor() as i32,
        (translation.z / 32.0).floor() as i32,
    ];
    for axis in 0..3 {
        camera_cell[axis] = camera_cell[axis].clamp(lo[axis], hi[axis]);
    }
    let in_domain = |cell: [i32; 3]| {
        (0..3).all(|axis| {
            cell[axis] >= lo[axis]
                && cell[axis] <= hi[axis]
                && (cell[axis] - camera_cell[axis]).abs() <= CULL_BFS_MAX_CHEBYSHEV
        })
    };

    // Node = (cell, dirs_used bitmask, entry face). dirs_used only gains bits
    // along a path and no-backtrack caps it at one direction per axis, so at
    // most 3 bits are ever set; with the entry face in the visited key that
    // bounds re-visits per cell to a small constant. The entry face IS part
    // of the key: two paths can reach a cell with identical dirs_used but
    // different entry faces, and the sets of exits the mask allows for those
    // entries differ — deduping them against each other would under-visit
    // (= holes in the world).
    let mut reached: HashSet<[i32; 3]> = HashSet::new();
    let mut visited: HashSet<([i32; 3], u8, u8)> = HashSet::new();
    let mut queue: VecDeque<([i32; 3], u8, u8)> = VecDeque::new();
    visited.insert((camera_cell, 0, NO_ENTRY));
    queue.push_back((camera_cell, 0, NO_ENTRY));
    while let Some((cell, dirs, entry)) = queue.pop_front() {
        reached.insert(cell);
        // Grid-absent cells and chunks not yet carrying a mask: fully open.
        let mask = grid.0.get(&cell).and_then(|&e| masks.get(e).ok()).map(|c| c.0);
        for d in 0..6u8 {
            // No-backtrack: never step opposite to a direction already used.
            if dirs & (1 << (d ^ 1)) != 0 {
                continue;
            }
            // Connectivity: the cell's own air must connect entry → exit face.
            // The start cell (no entry face) exits freely in every direction.
            if let Some(mask) = mask {
                // entry == d would make pair_bit's a<b debug_assert trip, but
                // it's unreachable: entering via `entry` put `entry^1 == d^1`
                // in dirs, and the no-backtrack check above already skipped d.
                debug_assert!(entry == NO_ENTRY || entry != d as u8);
                if entry != NO_ENTRY && mask & pair_bit(entry as u32, d as u32) == 0 {
                    continue;
                }
            }
            let step = DIR_STEPS[d as usize];
            let neighbor = [cell[0] + step[0], cell[1] + step[1], cell[2] + step[2]];
            if !in_domain(neighbor) {
                continue;
            }
            // Frustum: reject neighbors fully outside the camera's view. No
            // Frustum component (headless) ⇒ everything counts as inside.
            if let Some(frustum) = frustum {
                let center = Vec3::new(
                    neighbor[0] as f32 * 32.0 + 16.0,
                    neighbor[1] as f32 * 32.0 + 16.0,
                    neighbor[2] as f32 * 32.0 + 16.0,
                );
                let sphere = Sphere { center: center.into(), radius: 16.0 * 3f32.sqrt() };
                if !frustum.intersects_sphere(&sphere, false) {
                    continue;
                }
            }
            let node = (neighbor, dirs | (1 << d), d ^ 1);
            if visited.insert(node) {
                queue.push_back(node);
            }
        }
    }

    // Apply, writing only on change (like `dedupe_ghosts`) to avoid
    // change-detection churn.
    for (key, &entity) in grid.0.iter() {
        let Ok(mut vis) = chunks.get_mut(entity) else { continue };
        let want =
            if reached.contains(key) { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want {
            *vis = want;
        }
    }
}

/// Registers `cull_chunks` at its one correct slot: `PostUpdate`, after
/// `UpdateFrusta` and before `VisibilityPropagate`. The BFS must read the
/// frustum of the frame being rendered — scheduled in `Update` it read last
/// frame's (`update_frusta` runs in `PostUpdate`), so every camera turn
/// visibly culled faces that were already inside the FOV at the leading
/// screen edge — and its `Visibility` writes must still land before this
/// frame's propagation. Shared by production (`renderer/mod.rs`) and the
/// headless pipeline tests so the schedules cannot drift apart.
pub fn cull_plugin(app: &mut App) {
    use bevy::camera::visibility::VisibilitySystems;
    app.add_systems(
        PostUpdate,
        cull_chunks
            .after(VisibilitySystems::UpdateFrusta)
            .before(VisibilitySystems::VisibilityPropagate),
    );
}

/// F4 flips the cave-culling graph on/off at runtime.
pub fn toggle_culling(keys: Res<ButtonInput<KeyCode>>, mut config: ResMut<CullingConfig>) {
    if keys.just_pressed(KeyCode::F4) {
        config.graph_enabled = !config.graph_enabled;
        info!("cave culling: {}", if config.graph_enabled { "on" } else { "off" });
    }
}

/// Native-only: opt every camera into the depth prepass + HZB two-phase
/// occlusion culling (experimental in Bevy 0.16+; REQUIRES DepthPrepass or
/// the component is ignored; incompatible with deferred, which we don't use).
/// Chunk meshes already prepass correctly via the packed-format prepass
/// vertex shader (phase 2). Own commit: revert this one commit to disable.
#[cfg(not(target_arch = "wasm32"))]
pub fn enable_native_occlusion(
    mut commands: Commands,
    cameras: Query<
        Entity,
        (With<Camera3d>, Without<bevy::core_pipeline::prepass::DepthPrepass>),
    >,
) {
    for e in &cameras {
        commands.entity(e).insert((
            bevy::core_pipeline::prepass::DepthPrepass,
            bevy::render::experimental::occlusion_culling::OcclusionCulling,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::super::binary::Face;
    use super::*;
    use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

    fn interior(cells: &[[usize; 3]]) -> PaddedVoxels {
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for [x, y, z] in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(1));
        }
        PaddedVoxels::from_interior(&v)
    }

    fn solid_except(air: &[[usize; 3]]) -> PaddedVoxels {
        let mut v = vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT];
        for [x, y, z] in air {
            v[x + 32 * y + 32 * 32 * z] = Voxel::default();
        }
        PaddedVoxels::from_interior(&v)
    }

    #[test]
    fn pair_index_is_a_bijection_onto_0_15() {
        let mut seen = [false; 15];
        for a in 0..6u32 {
            for b in (a + 1)..6u32 {
                let i = pair_index(a, b) as usize;
                assert!(!seen[i], "pair ({a},{b}) collides at {i}");
                seen[i] = true;
            }
        }
        assert!(seen.iter().all(|&s| s), "all 15 indices hit");
    }

    #[test]
    fn empty_chunk_connects_everything() {
        assert_eq!(face_connectivity(&interior(&[])), ALL_CONNECTED);
    }

    #[test]
    fn solid_chunk_connects_nothing() {
        assert_eq!(face_connectivity(&solid_except(&[])), 0);
    }

    #[test]
    fn straight_tunnel_connects_exactly_its_two_faces() {
        // 1-voxel tunnel along X at y=5, z=5 through an otherwise solid chunk.
        let air: Vec<[usize; 3]> = (0..32).map(|x| [x, 5, 5]).collect();
        let mask = face_connectivity(&solid_except(&air));
        assert_eq!(mask, pair_bit(Face::PosX.index(), Face::NegX.index()));
    }

    #[test]
    fn l_tunnel_connects_exactly_its_corner_pair() {
        // Air along -X to center, then down to -Y: connects NegX <-> NegY only.
        let mut air: Vec<[usize; 3]> = (0..=16).map(|x| [x, 16, 16]).collect();
        air.extend((0..=16).map(|y| [16, y, 16]));
        let mask = face_connectivity(&solid_except(&air));
        assert_eq!(mask, pair_bit(Face::NegX.index(), Face::NegY.index()));
    }

    #[test]
    fn open_column_connects_vertical_pair() {
        let air: Vec<[usize; 3]> = (0..32).map(|y| [7, y, 9]).collect();
        let mask = face_connectivity(&solid_except(&air));
        assert_eq!(mask, pair_bit(Face::PosY.index(), Face::NegY.index()));
    }

    #[test]
    fn two_isolated_rooms_union_their_pairs_without_cross_connecting() {
        // Room A: tunnel X (touches ±X). Room B: tunnel Z (touches ±Z),
        // far apart, no shared air. Mask = both pairs, and NOT e.g. PosX<->PosZ.
        let mut air: Vec<[usize; 3]> = (0..32).map(|x| [x, 3, 3]).collect();
        air.extend((0..32).map(|z| [28, 28, z]));
        let mask = face_connectivity(&solid_except(&air));
        let expect = pair_bit(Face::PosX.index(), Face::NegX.index())
            | pair_bit(Face::PosZ.index(), Face::NegZ.index());
        assert_eq!(mask, expect);
    }

    #[test]
    fn interior_pocket_touching_no_face_adds_nothing() {
        // A sealed air pocket in the middle: connects no faces.
        let air: Vec<[usize; 3]> = vec![[15, 15, 15], [16, 15, 15]];
        assert_eq!(face_connectivity(&solid_except(&air)), 0);
    }

    // --- Camera BFS culling (headless pipeline scenes) -------------------
    //
    // These run the real meshing pipeline (`pipeline_app` registers the full
    // production chain, `cull_chunks` last) with a headless camera: explicit
    // `GlobalTransform` + active `Camera` (MinimalPlugins has no transform
    // propagation; a missing/default `Frustum` counts as "everything
    // inside"). Assertions are on END-STATES via `wait_until`, never on
    // transient scheduling intermediates — the mesher can spawn, complete,
    // and apply within a single update.

    use super::super::queue::tests::{
        pipeline_app, spawn_chunk, voxels_with, wait_until, wait_until_settled,
    };
    use super::super::queue::MeshBudgets;
    use crate::renderer::bridge::VoxelData;

    fn spawn_camera(app: &mut App, pos: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                Camera3d::default(),
                Camera { is_active: true, ..Default::default() },
                GlobalTransform::from(Transform::from_translation(pos)),
            ))
            .id()
    }

    fn vis(app: &App, e: Entity) -> Visibility {
        *app.world().entity(e).get::<Visibility>().expect("chunk has Visibility")
    }

    /// Camera in the (empty) cell (0,0,0), a fully solid chunk at cell
    /// (1,0,0), and a chunk with content at cell (2,0,0) behind it.
    fn wall_scene(app: &mut App) -> (Entity, Entity) {
        spawn_camera(app, Vec3::new(8.0, 8.0, 8.0));
        let wall = spawn_chunk(
            app,
            Vec3::ZERO,
            Vec3::new(32.0, 0.0, 0.0),
            vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT],
        );
        let behind =
            spawn_chunk(app, Vec3::ZERO, Vec3::new(64.0, 0.0, 0.0), voxels_with(&[[5, 5, 5]]));
        (wall, behind)
    }

    #[test]
    fn wall_hides_what_is_behind_it() {
        // [camera cell (0,0,0), empty] [solid wall at (1,0,0)] [chunk at (2,0,0)].
        // The wall's mask is 0: the BFS enters it but cannot pass through, and
        // every around-path (over/under/beside) is cut by the no-backtrack
        // rule, so (2,0,0) ends Hidden while the wall itself stays visible.
        let mut app = pipeline_app(MeshBudgets::default());
        let (wall, behind) = wall_scene(&mut app);
        assert!(
            wait_until(&mut app, |app| vis(app, behind) == Visibility::Hidden),
            "the chunk behind a solid wall must end Hidden"
        );
        assert_eq!(vis(&app, wall), Visibility::Inherited, "the wall itself is entered → visible");
    }

    #[test]
    fn open_air_gap_does_not_hide_distant_chunks() {
        // Chunks at (0,0,0) and (5,0,0) with NOTHING between them in the grid:
        // absent cells traverse as fully-open air, so the far chunk stays visible.
        let mut app = pipeline_app(MeshBudgets::default());
        spawn_camera(&mut app, Vec3::new(8.0, 8.0, 8.0));
        let near = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        let far =
            spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(160.0, 0.0, 0.0), voxels_with(&[[5, 5, 5]]));
        assert!(wait_until_settled(&mut app), "pipeline must settle");
        app.update();
        assert_eq!(vis(&app, near), Visibility::Inherited, "camera's own cell is always visible");
        assert_eq!(vis(&app, far), Visibility::Inherited, "grid-absent gap must not block sight");
    }

    #[test]
    fn opening_a_wall_reveals_the_room_behind_it() {
        // Spec §7.4 regression: an edit must update connectivity AND visibility.
        let mut app = pipeline_app(MeshBudgets::default());
        let (wall, behind) = wall_scene(&mut app);
        assert!(
            wait_until(&mut app, |app| vis(app, behind) == Visibility::Hidden),
            "the wall must hide the room first"
        );
        // Carve a straight X tunnel through the wall at y=5, z=5.
        {
            let mut voxels = app.world_mut().get_mut::<VoxelData>(wall).unwrap();
            for x in 0..32 {
                voxels.0[x + 32 * 5 + 32 * 32 * 5] = Voxel::default();
            }
        }
        assert!(
            wait_until(&mut app, |app| vis(app, behind) == Visibility::Inherited),
            "carving a tunnel must remesh the wall and reveal the room behind it"
        );
    }

    #[test]
    fn disabling_the_graph_restores_everything() {
        let mut app = pipeline_app(MeshBudgets::default());
        let (wall, behind) = wall_scene(&mut app);
        assert!(
            wait_until(&mut app, |app| vis(app, behind) == Visibility::Hidden),
            "the wall scene must hide the behind chunk first"
        );
        app.world_mut().resource_mut::<CullingConfig>().graph_enabled = false;
        app.update();
        assert_eq!(vis(&app, wall), Visibility::Inherited);
        assert_eq!(vis(&app, behind), Visibility::Inherited, "culling off ⇒ everything Inherited");
    }

    #[test]
    fn chunks_without_a_mask_yet_stay_visible_and_traversable() {
        // In-flight budget 0: `spawn_mesh_tasks` never runs, so no chunk ever
        // gets a `ChunkConnectivity`. The BFS must treat the (actually solid)
        // wall as open and NOT hide anything behind it — under-visibility is
        // the failure mode.
        let mut app =
            pipeline_app(MeshBudgets { max_in_flight: 0, uploads_per_frame: 0 });
        spawn_camera(&mut app, Vec3::new(8.0, 8.0, 8.0));
        let wall = spawn_chunk(
            &mut app,
            Vec3::ZERO,
            Vec3::new(32.0, 0.0, 0.0),
            vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT],
        );
        let behind =
            spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(64.0, 0.0, 0.0), voxels_with(&[[5, 5, 5]]));
        for _ in 0..3 {
            app.update();
        }
        assert!(
            app.world().entity(wall).get::<ChunkConnectivity>().is_none(),
            "premise: no mask may have landed with a zero meshing budget"
        );
        assert_eq!(vis(&app, wall), Visibility::Inherited, "mask-less chunk stays visible");
        assert_eq!(
            vis(&app, behind),
            Visibility::Inherited,
            "mask-less chunk must traverse as open — nothing behind it may hide"
        );
    }

    /// Regression: the cull BFS must use the CURRENT frame's frustum.
    /// `update_frusta` runs in `PostUpdate`, so a cull pass scheduled in
    /// `Update` reads last frame's frustum while the frame renders with this
    /// frame's — turning the camera visibly culls faces that are already
    /// back inside the FOV. With real frustum updates in the loop, turning
    /// toward a chunk must reveal it in the SAME frame.
    #[test]
    fn turning_toward_a_chunk_reveals_it_in_the_same_frame() {
        let mut app = pipeline_app(MeshBudgets::default());
        app.add_plugins((bevy::transform::TransformPlugin, bevy::camera::CameraProjectionPlugin));
        // Camera in cell (0,0,0) looking AWAY (-X) from a chunk at cell (2,0,0).
        let cam = app
            .world_mut()
            .spawn((
                Camera3d::default(),
                Camera { is_active: true, ..Default::default() },
                Projection::default(),
                Transform::from_translation(Vec3::new(8.0, 8.0, 8.0))
                    .looking_to(Vec3::NEG_X, Vec3::Y),
            ))
            .id();
        let chunk =
            spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(64.0, 0.0, 0.0), voxels_with(&[[5, 5, 5]]));
        assert!(
            wait_until(&mut app, |app| vis(app, chunk) == Visibility::Hidden),
            "premise: a chunk fully behind the camera ends Hidden"
        );
        // Turn to face the chunk, then run exactly ONE frame.
        app.world_mut().entity_mut(cam).get_mut::<Transform>().unwrap().look_to(Vec3::X, Vec3::Y);
        app.update();
        assert_eq!(
            vis(&app, chunk),
            Visibility::Inherited,
            "a chunk inside the new frustum must show the same frame it enters the FOV"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_cameras_get_depth_prepass_and_occlusion_culling() {
        use bevy::core_pipeline::prepass::DepthPrepass;
        use bevy::render::experimental::occlusion_culling::OcclusionCulling;

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_systems(Update, enable_native_occlusion);
        let cam = app.world_mut().spawn(Camera3d::default()).id();
        app.update();
        assert!(app.world().entity(cam).contains::<DepthPrepass>());
        assert!(app.world().entity(cam).contains::<OcclusionCulling>());
    }
}
