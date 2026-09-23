use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use crossbeam::channel::Receiver;
use game::{ClientUpdateEvent, GameData, GameDataUpdate, GameDataUpdateKind, RegionId};
use game::{EntityKey, Voxel};

use super::convert::{iso_to_transform, perspective_to_projection};
use super::hud::HudStatus;
use super::lod_rings;
use super::{ClientUpdates, LocalPlayer};

#[derive(Resource, Default)]
pub struct Regions(pub BTreeMap<RegionId, Receiver<GameDataUpdate>>);

#[derive(Resource, Default)]
pub struct RegionRoots(pub BTreeMap<RegionId, Entity>);

#[derive(Resource, Default)]
pub struct SimEntityMap(pub BTreeMap<(RegionId, EntityKey), Entity>);

// identity breadcrumbs for debugging; not yet read
#[allow(dead_code)]
#[derive(Component)]
pub struct SimEntity {
    pub region: RegionId,
    pub key: EntityKey,
}

/// Where the sim says this entity should be. `Transform` chases it (task 5).
/// Written by the bridge on every Do *and* Undo event: target writes are
/// last-write-wins, so a rollback/reapply burst drained in one frame
/// collapses to the final pose. That idempotence is the invariant that
/// makes the one-way bridge safe — do not switch to incremental deltas.
/// Per-frame lerp factor for the camera's *position*. Kept < 1.0 so the
/// eye glides between 50Hz sim poses instead of stepping. Rotation is a
/// separate factor (see `camera`) held at 1.0 so the look stays instant.
pub const CAMERA_POS_SMOOTHING: f32 = 0.3;

#[derive(Component)]
pub struct SimTarget {
    pub pos: Vec3,
    pub rot: Quat,
    pub pos_smoothing: f32,
    pub rot_smoothing: f32,
    pub pos_snap: f32,
    pub rot_snap: f32,
}

impl SimTarget {
    // pos_snap values are world-unit distances (1 unit = 1/16 m); smoothing
    // factors and rotation snaps are dimensionless/radians.
    pub fn body(pos: Vec3, rot: Quat) -> Self {
        Self { pos, rot, pos_smoothing: 0.5, rot_smoothing: 0.5, pos_snap: 1.6, rot_snap: 0.1 }
    }
    pub fn camera(pos: Vec3, rot: Quat) -> Self {
        // The render camera SNAPS its rotation (rot_smoothing = 1.0) so
        // there's no slerp-arc tilt during turns, but LERPS its position so
        // movement glides instead of stepping. The snapped per-tick sim look
        // is then overwritten by the per-frame local look (`look.rs`).
        Self {
            pos,
            rot,
            pos_smoothing: CAMERA_POS_SMOOTHING,
            rot_smoothing: 1.0,
            pos_snap: 0.008,
            rot_snap: 0.001,
        }
    }
}

/// A chunk's 32³ voxel grid, run-length encoded along the linear index
/// (`x + 32*y + 32*32*z`, so runs extend along +X).
///
/// The dense form is 64 KiB (32768 voxels × 2 B) and the client used to keep one
/// alive per chunk for the whole session — at ~2800 chunks per region that was
/// ~183 MB per region, three quarters of the client's heap, and the largest
/// single consumer by an order of magnitude.
///
/// Runs are cheap here by construction: a chunk is 8 blocks of 16³, each uniform
/// unless chiselled, so every run is ≥16 voxels and a plain air/stone chunk is
/// one run. Nothing needs the dense array persistently — the mesher expands it
/// transiently to build `PaddedVoxels`, then drops it — so we store the runs and
/// hand out a dense `Vec` on demand.
#[derive(Component, Clone, PartialEq, Eq, Debug)]
pub struct VoxelData(Vec<(u16, game::BlockId)>);

impl VoxelData {
    /// Encode a dense 32³ grid.
    pub fn new(voxels: impl AsRef<[Voxel]>) -> Self {
        let voxels = voxels.as_ref();
        debug_assert_eq!(voxels.len(), game::CHUNK_VOXEL_COUNT);
        let mut runs: Vec<(u16, game::BlockId)> = Vec::new();
        for v in voxels {
            match runs.last_mut() {
                // A run can't overflow u16: the whole chunk is 32768 voxels.
                Some((len, id)) if *id == v.block => *len += 1,
                _ => runs.push((1, v.block)),
            }
        }
        Self(runs)
    }

    /// Expand back to the dense 32³ grid the mesher consumes.
    pub fn decode(&self) -> Vec<Voxel> {
        let mut out = Vec::with_capacity(game::CHUNK_VOXEL_COUNT);
        for &(len, id) in &self.0 {
            out.extend(std::iter::repeat_n(Voxel::new(id), len as usize));
        }
        out
    }

    /// Visit monotonically increasing dense-grid indices without expanding the
    /// RLE component. The meshing snapshot builder uses this for interiors and
    /// neighbor boundary zones, where requested indices naturally arrive in
    /// linear x-major order.
    pub(crate) fn for_each_index_sorted(
        &self,
        indices: impl IntoIterator<Item = usize>,
        mut visit: impl FnMut(usize, Voxel),
    ) {
        let mut runs = self.0.iter();
        let Some(&(mut run_len, mut block)) = runs.next() else {
            return;
        };
        let mut run_end = run_len as usize;
        let mut previous = None;
        for index in indices {
            debug_assert!(previous.is_none_or(|p| index >= p), "RLE indices must be sorted");
            debug_assert!(index < game::CHUNK_VOXEL_COUNT, "voxel index out of bounds");
            previous = Some(index);
            while index >= run_end {
                let &(next_len, next_block) = runs.next().expect("RLE covers the full chunk");
                run_len = next_len;
                block = next_block;
                run_end += run_len as usize;
            }
            visit(index, Voxel::new(block));
        }
    }

    /// Decode, mutate, re-encode. The edit path is rare (a chisel or a block
    /// break); the read path is what had to stay cheap.
    pub fn edit(&mut self, f: impl FnOnce(&mut Vec<Voxel>)) {
        let mut dense = self.decode();
        f(&mut dense);
        *self = Self::new(dense);
    }

    /// Number of runs — the thing that actually costs memory.
    pub fn runs(&self) -> usize {
        self.0.len()
    }
}

/// Mirror of the sim's `EntityKind` for this entity. Consumed by the avatar
/// system to attach a renderable mesh.
#[derive(Component, Clone, Copy)]
pub struct SimKind(pub game::EntityKind);

/// Marks a bevy entity as a ghost mirror sourced from another region. Used
/// only for render dedupe: when the source region is also loaded locally,
/// the owned copy renders and the ghost hides (sim state keeps both).
#[derive(Component, Clone, Copy)]
pub struct GhostSource(pub RegionId);

/// Chunks derived per snapshot-voxel task. A region is ~2800 chunks at
/// ~0.3 ms each, so a batch is ~20 ms of worker time: small enough that the
/// first terrain shows up quickly and (on wasm, where tasks run on the main
/// thread) that no single task is a long hitch.
pub const SNAPSHOT_VOXEL_TASK_CHUNKS: usize = 64;

/// Floor on derived snapshot voxels handed to chunk entities per frame (the
/// live count follows [`SNAPSHOT_VOXELS_PER_SECOND`]). Each insert makes
/// `mark_remesh_dirty` fingerprint that chunk (~0.15 ms on a power-clamped
/// machine, far less otherwise), so this is the main-thread cost of a joining
/// region.
pub const SNAPSHOT_VOXELS_PER_FRAME: usize = 64;
/// Target insert rate. Kept above the mesher's `MESH_UPLOADS_PER_SECOND` so
/// voxels always arrive faster than they can be meshed: the remesh queue then
/// holds everything pending and its nearest-first ranking decides the order.
pub const SNAPSHOT_VOXELS_PER_SECOND: f32 = 4800.0;
/// Ceiling per frame: twice a 60 Hz frame's worth, so a long frame cannot
/// snowball (each insert costs `mark_remesh_dirty` ~45 us unclamped; 512 per
/// frame measured 31 ms stalls). See `MAX_MESH_UPLOADS_PER_FRAME`.
pub const MAX_SNAPSHOT_VOXELS_PER_FRAME: usize = 160;

/// A chunk entity spawned from a region snapshot whose `VoxelData` is still
/// being derived off-thread. Removed when the voxels land — or when an
/// incremental `SetVoxelComponent` gets there first, which then wins: the
/// late snapshot result is older and must not overwrite it.
#[derive(Component)]
pub struct PendingSnapshotVoxels;

/// One batch of snapshot-voxel derivation. Child of the region root, so
/// removing (or replacing) the region despawns — and thereby cancels — it.
#[derive(Component)]
pub struct SnapshotVoxelTask(Task<Vec<(Entity, VoxelData)>>);

/// Derived voxels waiting for their per-frame insert budget.
#[derive(Resource, Default)]
pub struct SnapshotVoxelQueue {
    ready: VecDeque<(Entity, VoxelData)>,
    /// Inserted since the queue was last empty (for the `[perf]` line).
    applied: u32,
}

/// Collect finished snapshot-voxel batches and insert up to
/// [`SNAPSHOT_VOXELS_PER_FRAME`] of them. Spawning a region used to derive and
/// RLE-encode every chunk inside `drain_client_updates`: one 0.5-2 s frame per
/// region, 8.9 s for the production 3x3 join (2026-09-19 capture).
pub fn apply_snapshot_voxels(
    mut commands: Commands,
    time: Res<Time<Real>>,
    mut tasks: Query<(Entity, &mut SnapshotVoxelTask)>,
    pending: Query<(), With<PendingSnapshotVoxels>>,
    mut queue: ResMut<SnapshotVoxelQueue>,
) {
    for (task_entity, mut task) in &mut tasks {
        if let Some(batch) = block_on(future::poll_once(&mut task.0)) {
            queue.ready.extend(batch);
            commands.entity(task_entity).despawn();
        }
    }
    let mut budget = super::meshing::queue::per_frame_budget(
        SNAPSHOT_VOXELS_PER_SECOND,
        time.delta(),
        SNAPSHOT_VOXELS_PER_FRAME,
        MAX_SNAPSHOT_VOXELS_PER_FRAME,
    );
    while budget > 0 {
        let Some((entity, voxels)) = queue.ready.pop_front() else {
            break;
        };
        // No marker: the entity is gone (region removed/replaced) or an
        // incremental update already wrote newer voxels. Costs no budget.
        if !pending.contains(entity) {
            continue;
        }
        commands
            .entity(entity)
            .insert(voxels)
            .remove::<PendingSnapshotVoxels>();
        queue.applied += 1;
        budget -= 1;
    }
    if queue.applied > 0 && queue.ready.is_empty() && tasks.is_empty() {
        game::perf_event!("snapshot_voxels_applied", "chunks={}", queue.applied);
        queue.applied = 0;
    }
}

pub fn drain_client_updates(
    mut commands: Commands,
    updates: Res<ClientUpdates>,
    mut player: ResMut<LocalPlayer>,
    mut regions: ResMut<Regions>,
    mut roots: ResMut<RegionRoots>,
    mut map: ResMut<SimEntityMap>,
    mut hud_status: ResMut<HudStatus>,
    mut ui_updates: ResMut<crate::renderer::ui_bridge::UiUpdates>,
    mut debug_lines: ResMut<crate::renderer::collider_debug::DebugColliderLines>,
    mut lod_rings: ResMut<lod_rings::LodRings>,
) {
    // Unbudgeted: drains everything the game thread queued since last frame.
    let mut stall = game::perf::StallGuard::new("drain_client_updates");
    while let Ok(event) = updates.0.try_recv() {
        stall.add_work(1);
        match event {
            ClientUpdateEvent::NewRegion(id, data, receiver) => {
                if roots.0.contains_key(&id) {
                    // Replace-on-re-receipt: resubscribe/crash-respawn resync.
                    remove_region(&mut commands, &mut regions, &mut roots, &mut map, id);
                }
                let offset = id.world_offset();
                let root = commands
                    .spawn((
                        Transform::from_translation(Vec3::new(offset[0], offset[1], offset[2])),
                        Visibility::default(),
                        Name::new(format!("region {:?}", id)),
                    ))
                    .id();
                roots.0.insert(id, root);
                regions.0.insert(id, receiver);
                let spawn = game::perf::Stopwatch::start();
                let data = Arc::new(data);
                spawn_region_snapshot(&mut commands, root, id, &data, &mut map, player.0);
                // Main-thread cost of one region: an entity per sim entity.
                // The per-chunk voxel derivation runs off-thread (see
                // `apply_snapshot_voxels`).
                game::perf_event!(
                    "bridge_region_spawned",
                    "region={},{} spawn_ms={} entities={} chunks={} regions={}",
                    id.x,
                    id.z,
                    game::perf::ms(spawn.elapsed()),
                    data.ecs.entities.len(),
                    data.ecs.chunk.iter().filter(|(_, chunk)| chunk.is_some()).count(),
                    roots.0.len()
                );
                info!("bridge: region {:?} loaded", id);
            }
            ClientUpdateEvent::SetPlayer(client_id) => player.0 = Some(client_id),
            ClientUpdateEvent::RemoveRegion(id) => {
                remove_region(&mut commands, &mut regions, &mut roots, &mut map, id);
                debug_lines.0.remove(&id);
                info!("bridge: region {:?} removed", id);
            }
            ClientUpdateEvent::LodData(key, payload) => {
                lod_rings.pending.push(lod_rings::LodEventMsg::Data(key, payload));
            }
            ClientUpdateEvent::RemoveLod(key) => {
                lod_rings.pending.push(lod_rings::LodEventMsg::Remove(key));
            }
            ClientUpdateEvent::GameCrash(e) => error!("bridge: game thread crashed: {:?}", e),
            ClientUpdateEvent::HudStatus { home_region, viewer_region, connection } => {
                hud_status.home_region = Some(home_region);
                hud_status.viewer_region = Some(viewer_region);
                hud_status.connection = connection;
            }
            ClientUpdateEvent::UiChannel(recv) => {
                ui_updates.0 = Some(recv);
            }
            ClientUpdateEvent::DebugLines(id, lines) => {
                debug_lines.0.insert(id, lines);
            }
        }
    }
}

/// Tear down everything the bridge built for a region: root entity tree,
/// update receiver, entity map entries. Also the first half of
/// replace-on-re-receipt (crash-respawn resync).
fn remove_region(
    commands: &mut Commands,
    regions: &mut Regions,
    roots: &mut RegionRoots,
    map: &mut SimEntityMap,
    id: RegionId,
) {
    if let Some(root) = roots.0.remove(&id) {
        // despawn() removes descendants via ChildOf relationships (Bevy 0.16+).
        commands.entity(root).despawn();
    }
    regions.0.remove(&id);
    map.0.retain(|(region, _), _| *region != id);
}

/// Port of the old `TrueWorld::new` snapshot walk.
fn spawn_region_snapshot(
    commands: &mut Commands,
    root: Entity,
    region: RegionId,
    data: &Arc<GameData>,
    map: &mut SimEntityMap,
    local_player: Option<game::ClientId>,
) {
    let mut chunk_entities: Vec<(Entity, EntityKey)> = Vec::new();
    // Every player in the snapshot has a sim camera, but only the local
    // player's may become this window's active Camera3d.
    let local_camera_key = local_player.and_then(|id| data.player_entites.get(&id).copied());
    for (key, _) in data.ecs.entities.iter() {
        let mut e = commands.spawn((
            SimEntity { region, key },
            Transform::IDENTITY,
            Visibility::default(),
            ChildOf(root),
        ));
        if let Some(handle) = data.ecs.rigidbody.try_get(key) {
            if let Some(body) = data.physics.bodies.get(*handle) {
                let tf = iso_to_transform(body.position());
                e.insert((tf, SimTarget::body(tf.translation, tf.rotation)));
            }
        }
        if data.ecs.chunk.try_get(key).is_some() {
            e.insert(PendingSnapshotVoxels);
            chunk_entities.push((e.id(), key));
        }
        if let Some(cam) = data.ecs.camera.try_get(key) {
            if Some(key) == local_camera_key {
                if let Some(handle) = cam.view_matrix {
                    if let Some(body) = data.physics.bodies.get(handle) {
                        let tf = iso_to_transform(body.position());
                        e.insert((
                            Camera3d::default(),
                            Projection::Perspective(perspective_to_projection(&cam.proj_matrix)),
                            tf,
                            SimTarget::camera(tf.translation, tf.rotation),
                            bevy::ui::IsDefaultUiCamera,
                        ));
                    }
                }
            }
            // Other players' pose tracking comes from the rigidbody branch
            // above; their cameras stay inert in this window.
        }
        if let Some(kind) = *data.ecs.kind.try_get(key) {
            e.insert(SimKind(kind));
        }
        map.0.insert((region, key), e.id());
    }

    // Derive + RLE-encode the chunks' voxels off the main thread, in batches
    // that share the snapshot through the `Arc` (no per-chunk clone).
    let pool = AsyncComputeTaskPool::get();
    for batch in chunk_entities.chunks(SNAPSHOT_VOXEL_TASK_CHUNKS) {
        let batch = batch.to_vec();
        let data = Arc::clone(data);
        let task = pool.spawn(async move {
            batch
                .into_iter()
                .filter_map(|(entity, key)| {
                    let chunk = data.ecs.chunk.try_get(key).as_ref()?;
                    let voxels = game::derive_voxels(&chunk.blocks, &chunk.chisel);
                    Some((entity, VoxelData::new(voxels)))
                })
                .collect()
        });
        commands.spawn((SnapshotVoxelTask(task), ChildOf(root)));
    }
}

pub fn drain_region_updates(
    mut commands: Commands,
    regions: Res<Regions>,
    roots: Res<RegionRoots>,
    mut map: ResMut<SimEntityMap>,
    mut targets: Query<&mut SimTarget>,
    local_player: Res<LocalPlayer>,
    mut windows: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    for (&region, receiver) in regions.0.iter() {
        let root = *roots.0.get(&region).expect("RegionRoots co-populated with Regions");
        while let Ok(update) = receiver.try_recv() {
            // Do and Undo both applied: see SimTarget doc comment.
            match update.update_kind {
                GameDataUpdateKind::CreateEntity(key) => {
                    let e = commands
                        .spawn((
                            SimEntity { region, key },
                            Transform::IDENTITY,
                            Visibility::default(),
                            SimTarget::body(Vec3::ZERO, Quat::IDENTITY),
                            ChildOf(root),
                        ))
                        .id();
                    map.0.insert((region, key), e);
                }
                GameDataUpdateKind::RemoveEntity(key) => {
                    if let Some(e) = map.0.remove(&(region, key)) {
                        commands.entity(e).despawn();
                    } else {
                        warn!("bridge: RemoveEntity for unmapped {:?}", key);
                    }
                }
                GameDataUpdateKind::SetEntityPosition(key, iso) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetEntityPosition for unmapped {:?}", key);
                        continue;
                    };
                    let tf = iso_to_transform(&iso);
                    if let Ok(mut target) = targets.get_mut(e) {
                        target.pos = tf.translation;
                        target.rot = tf.rotation;
                    } else {
                        // Entity spawned via Commands earlier this same drain —
                        // not yet queryable. Insert (overwrites on apply).
                        commands.entity(e).insert(SimTarget::body(tf.translation, tf.rotation));
                    }
                }
                GameDataUpdateKind::SetVoxelComponent(key, Some(voxels)) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        // Newer than any snapshot voxels still in flight.
                        commands
                            .entity(e)
                            .insert(VoxelData::new(voxels))
                            .remove::<PendingSnapshotVoxels>();
                    } else {
                        warn!("bridge: SetVoxelComponent for unmapped {:?}", key);
                    }
                }
                GameDataUpdateKind::SetVoxelComponent(key, None) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands
                            .entity(e)
                            .remove::<(VoxelData, PendingSnapshotVoxels)>();
                    } else {
                        warn!("bridge: SetVoxelComponent for unmapped {:?}", key);
                    }
                }
                GameDataUpdateKind::AddCameraComponent(key, client_id, proj, iso) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: AddCameraComponent for unmapped {:?}", key);
                        continue;
                    };
                    let tf = iso_to_transform(&iso);
                    if local_player.0 == Some(client_id) {
                        commands.entity(e).insert((
                            Camera3d::default(),
                            Projection::Perspective(perspective_to_projection(&proj)),
                            tf,
                            SimTarget::camera(tf.translation, tf.rotation),
                            bevy::ui::IsDefaultUiCamera,
                        ));
                    } else {
                        // Another player's camera: track its pose so the
                        // entity follows the sim, but never render from it.
                        commands.entity(e).insert((tf, SimTarget::body(tf.translation, tf.rotation)));
                    }
                }
                GameDataUpdateKind::RemoveCameraComponent(key) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).remove::<(Camera3d, Projection)>();
                    } else {
                        warn!("bridge: RemoveCameraComponent for unmapped {:?}", key);
                    }
                }
                GameDataUpdateKind::UpdateCameraViewProj(key, proj) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).insert(Projection::Perspective(perspective_to_projection(&proj)));
                    } else {
                        warn!("bridge: UpdateCameraViewProj for unmapped {:?}", key);
                    }
                }
                GameDataUpdateKind::UpdateCameraViewMatrix(key, iso) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: UpdateCameraViewMatrix for unmapped {:?}", key);
                        continue;
                    };
                    let tf = iso_to_transform(&iso);
                    if let Ok(mut target) = targets.get_mut(e) {
                        target.pos = tf.translation;
                        target.rot = tf.rotation;
                    } else {
                        commands.entity(e).insert(SimTarget::camera(tf.translation, tf.rotation));
                    }
                }
                GameDataUpdateKind::SetEntityKind(key, kind) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetEntityKind for unmapped {:?}", key);
                        continue;
                    };
                    match kind {
                        Some(k) => { commands.entity(e).insert(SimKind(k)); }
                        None => { commands.entity(e).remove::<SimKind>(); }
                    }
                }
                GameDataUpdateKind::SetGhostSource(key, src) => {
                    let Some(&e) = map.0.get(&(region, key)) else {
                        warn!("bridge: SetGhostSource for unmapped {:?}", key);
                        continue;
                    };
                    match src {
                        Some(rc) => { commands.entity(e).insert(GhostSource(rc)); }
                        None => { commands.entity(e).remove::<GhostSource>(); }
                    }
                }
                GameDataUpdateKind::SetFreeCam(client_id, enabled) => {
                    // Only the local player's toggle may grab this window's cursor.
                    if local_player.0 != Some(client_id) {
                        continue;
                    }
                    let (want_grab, want_visible) = if enabled {
                        (CursorGrabMode::Locked, false)
                    } else {
                        (CursorGrabMode::None, true)
                    };
                    if let Ok(mut cursor) = windows.single_mut() {
                        // Idempotent: SetFreeCam is emitted Do-only (no Undo
                        // pairing), so every reconcile that replays the toggle
                        // tick re-delivers the SAME value. `single_mut()` + a
                        // field write trips Bevy change detection unconditionally,
                        // which re-issues the winit cursor grab each time — an X11
                        // re-grab loop ("already confined by another client" spam)
                        // when the grab is contended. Only write on a real change.
                        if cursor.grab_mode != want_grab || cursor.visible != want_visible {
                            cursor.grab_mode = want_grab;
                            cursor.visible = want_visible;
                        }
                    }
                }
            }
        }
    }
}

/// Render rule: a ghost is visible only while its source region is NOT
/// loaded locally — with both regions in the window you see the owned copy,
/// not its mirror. Runs every frame; ghosts are few.
pub fn dedupe_ghosts(
    roots: Res<RegionRoots>,
    mut ghosts: Query<(&GhostSource, &mut Visibility)>,
) {
    for (src, mut vis) in ghosts.iter_mut() {
        let hidden = roots.0.contains_key(&src.0);
        let want = if hidden { Visibility::Hidden } else { Visibility::Inherited };
        if *vis != want {
            *vis = want;
        }
    }
}

/// Enforce exactly one active render camera: the local player's camera in the
/// *home* region. During a boundary handoff the player is briefly owned by both
/// the outgoing and incoming region, so both spawn a local `Camera3d` +
/// `IsDefaultUiCamera`. Two active cameras with equal priority make Bevy pick
/// ambiguously per frame (view flickers; mouse-look/HUD bind to the wrong one).
///
/// Rather than remove `Camera3d` (which would need fragile re-add logic when
/// the player crosses back into a still-loaded region), this toggles
/// `Camera::is_active` and moves the single `IsDefaultUiCamera` marker so only
/// the home camera renders. All `Camera3d` entities here are the local player's
/// (the bridge never gives another player a `Camera3d`), so no ownership filter
/// is needed. Falls back to a deterministic keeper if no camera sits in the
/// home region yet (handoff mid-flight) to avoid a black screen or flicker.
pub fn enforce_single_active_camera(
    mut commands: Commands,
    hud_status: Res<HudStatus>,
    mut cams: Query<(Entity, &SimEntity, &mut Camera, Has<bevy::ui::IsDefaultUiCamera>), With<Camera3d>>,
) {
    let home = hud_status.home_region;
    // Choose the single keeper: the home-region camera if one exists, else a
    // deterministic fallback (lowest Entity) so the pick doesn't flip frame to
    // frame while a handoff is mid-flight (which would itself flicker).
    let mut home_keeper: Option<Entity> = None;
    let mut fallback: Option<Entity> = None;
    for (e, se, _, _) in cams.iter() {
        if fallback.is_none_or(|f| e < f) {
            fallback = Some(e);
        }
        if Some(se.region) == home && home_keeper.is_none_or(|k| e < k) {
            home_keeper = Some(e);
        }
    }
    let Some(keeper) = home_keeper.or(fallback) else { return };

    for (e, _se, mut cam, has_marker) in cams.iter_mut() {
        let active = e == keeper;
        if cam.is_active != active {
            cam.is_active = active;
        }
        match (active, has_marker) {
            (true, false) => {
                commands.entity(e).insert(bevy::ui::IsDefaultUiCamera);
            }
            (false, true) => {
                commands.entity(e).remove::<bevy::ui::IsDefaultUiCamera>();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{ClientUpdates, GameEvents, LocalPlayer};
    use game::{ClientUpdateEvent, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, RegionCoords, Rollback};
    use game::EntityKey;
    use slotmapd::KeyData;

    fn app_shell() -> (App, crossbeam::channel::Sender<ClientUpdateEvent>) {
        let (client_send, client_recv) = crossbeam::channel::unbounded();
        let (game_send, _game_recv) = crossbeam::channel::unbounded();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(ClientUpdates(client_recv))
            .insert_resource(GameEvents(game_send))
            .init_resource::<LocalPlayer>()
            .init_resource::<Regions>()
            .init_resource::<RegionRoots>()
            .init_resource::<SimEntityMap>()
            .init_resource::<HudStatus>()
            .init_resource::<crate::renderer::ui_bridge::UiUpdates>()
            .init_resource::<crate::renderer::collider_debug::DebugColliderLines>()
            .init_resource::<lod_rings::LodRings>()
            .init_resource::<SnapshotVoxelQueue>()
            .add_systems(
                PreUpdate,
                (drain_client_updates, drain_region_updates, apply_snapshot_voxels).chain(),
            );
        (app, client_send)
    }

    fn test_app() -> (App, crossbeam::channel::Sender<ClientUpdateEvent>, crossbeam::channel::Sender<GameDataUpdate>, game::RegionId) {
        let (app, client_send) = app_shell();
        let (update_send, update_recv) = crossbeam::channel::unbounded();
        let region_id = RegionCoords::new(0, 0);
        let rb = Rollback::new(None);
        client_send
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv))
            .unwrap();
        (app, client_send, update_send, region_id)
    }

    fn test_proj() -> game::na::Perspective3<game::parry::math::Real> {
        game::na::Perspective3::new(
            game::parry::math::Real::from(1.5),
            game::parry::math::Real::from(1.2),
            game::parry::math::Real::from(0.1),
            game::parry::math::Real::from(100.0),
        )
    }

    fn key(n: u64) -> EntityKey {
        EntityKey::from(KeyData::from_ffi((1 << 32) | n))
    }

    #[test]
    fn new_region_spawns_root() {
        let (mut app, _c, _u, region_id) = test_app();
        app.update();
        let roots = app.world().resource::<RegionRoots>();
        assert!(roots.0.contains_key(&region_id));
    }

    #[test]
    fn create_set_position_remove() {
        let (mut app, _c, updates, region_id) = test_app();
        app.update();

        let k = key(7);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetEntityPosition(k, game::IsometryReal::identity()))).unwrap();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).expect("entity mapped");
        assert!(app.world().entity(e).contains::<SimTarget>());
        let target = app.world().entity(e).get::<SimTarget>().expect("SimTarget present");
        assert_eq!(target.pos, Vec3::ZERO, "identity isometry should convert to zero translation");
        assert_eq!(target.rot, Quat::IDENTITY, "identity isometry should convert to identity rotation");

        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::RemoveEntity(k))).unwrap();
        app.update();
        assert!(app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).is_none());
        assert!(app.world().get_entity(e).is_err(), "despawned");
    }

    #[test]
    fn unknown_key_is_tolerated() {
        let (mut app, _c, updates, _region_id) = test_app();
        app.update();
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::SetEntityPosition(key(99), game::IsometryReal::identity()))).unwrap();
        app.update(); // must not panic
    }

    #[test]
    fn camera_add_update_remove() {
        let (mut app, client, updates, region_id) = test_app();
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        app.update();
        let k = key(3);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::AddCameraComponent(k, 0, test_proj(), game::IsometryReal::identity()),
        )).unwrap();
        app.update();
        // second frame: AddCameraComponent on a same-drain-spawned entity goes through Commands
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        assert!(app.world().entity(e).contains::<Camera3d>());
        assert!(
            app.world().entity(e).contains::<bevy::ui::IsDefaultUiCamera>(),
            "local player's camera must be the default UI camera"
        );
        let Projection::Perspective(p) = app.world().entity(e).get::<Projection>().unwrap() else {
            panic!("expected perspective projection");
        };
        assert!((p.fov - 1.2).abs() < 1e-6);

        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::RemoveCameraComponent(k))).unwrap();
        app.update();
        assert!(!app.world().entity(e).contains::<Camera3d>());
    }

    #[test]
    fn foreign_camera_is_tracked_but_not_activated() {
        let (mut app, client, updates, region_id) = test_app();
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        app.update();
        let k = key(4);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            // client 1's camera arriving at client 0's window
            GameDataUpdateKind::AddCameraComponent(k, 1, test_proj(), game::IsometryReal::identity()),
        )).unwrap();
        app.update();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        assert!(
            !app.world().entity(e).contains::<Camera3d>(),
            "another player's camera must not render this window"
        );
        assert!(
            app.world().entity(e).contains::<SimTarget>(),
            "foreign player's pose must still be tracked"
        );
    }

    #[test]
    fn set_entity_kind_mirrors_to_sim_kind() {
        let (mut app, _c, updates, region_id) = test_app();
        app.update();
        let k = key(5);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(k, Some(game::EntityKind::Player)),
        )).unwrap();
        app.update();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        assert!(app.world().entity(e).contains::<SimKind>());

        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(k, None),
        )).unwrap();
        app.update();
        assert!(!app.world().entity(e).contains::<SimKind>());
    }

    #[test]
    fn snapshot_carries_entity_kind() {
        let (mut app, client) = app_shell();
        let (_update_send, update_recv) = crossbeam::channel::unbounded();
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        rb.create_player_safe(0);
        rb.create_player_safe(1);
        let data = (*rb.data).clone();
        let k0 = *data.player_entites.get(&0).unwrap();
        let k1 = *data.player_entites.get(&1).unwrap();

        let region_id = RegionCoords::new(0, 0);
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        app.update();

        let map = app.world().resource::<SimEntityMap>();
        let e0 = *map.0.get(&(region_id, k0)).unwrap();
        let e1 = *map.0.get(&(region_id, k1)).unwrap();
        assert!(app.world().entity(e0).contains::<SimKind>());
        assert!(app.world().entity(e1).contains::<SimKind>());
    }

    /// A snapshot with one terrain chunk; returns the chunk's sim key and the
    /// voxels its `VoxelData` must eventually hold.
    fn chunk_snapshot() -> (game::GameData, EntityKey, VoxelData) {
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        let chunk = game::Chunk::flat_floor(8);
        let expected = VoxelData::new(game::derive_voxels(&chunk.blocks, &chunk.chisel));
        let key = rb.create_mesh(game::ChunkCoords::new(0, 0, 0), chunk);
        ((*rb.data).clone(), key, expected)
    }

    fn wait_for(app: &mut App, mut done: impl FnMut(&App) -> bool) -> bool {
        for _ in 0..500 {
            app.update();
            if done(app) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        false
    }

    /// Voxel derivation left `drain_client_updates` (it was a 0.5-2 s frame per
    /// region): the chunk entity exists at once, flagged pending, and its
    /// voxels land a few frames later from the task pool.
    #[test]
    fn snapshot_chunk_voxels_arrive_off_thread_and_clear_the_marker() {
        let (mut app, client) = app_shell();
        let (_update_send, update_recv) = crossbeam::channel::unbounded();
        let (data, key, expected) = chunk_snapshot();
        let region_id = RegionCoords::new(0, 0);
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, key)).expect("chunk mapped");
        assert!(
            wait_for(&mut app, |app| app.world().entity(e).contains::<VoxelData>()),
            "snapshot voxels never arrived"
        );
        assert_eq!(app.world().entity(e).get::<VoxelData>(), Some(&expected));
        assert!(!app.world().entity(e).contains::<PendingSnapshotVoxels>());
        let mut tasks = app.world_mut().query::<&SnapshotVoxelTask>();
        assert_eq!(tasks.iter(app.world()).count(), 0, "finished task entities are despawned");
    }

    /// An edit that reaches the bridge before the derived snapshot voxels do is
    /// NEWER than them; the late result must be dropped, not applied on top.
    #[test]
    fn incremental_voxel_update_beats_the_late_snapshot_result() {
        let (mut app, client) = app_shell();
        let (update_send, update_recv) = crossbeam::channel::unbounded();
        let (data, key, snapshot_voxels) = chunk_snapshot();
        let region_id = RegionCoords::new(0, 0);
        let edited = vec![Voxel::new(game::BlockId(9)); game::CHUNK_VOXEL_COUNT];
        assert_ne!(VoxelData::new(&edited), snapshot_voxels);
        // Same frame as the snapshot: independent of how fast the task runs.
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        update_send
            .send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetVoxelComponent(key, Some(edited.clone())),
            ))
            .unwrap();
        app.update();

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, key)).expect("chunk mapped");
        // Let the task finish and the queue drain, then look at what survived.
        let mut drained = false;
        for _ in 0..500 {
            app.update();
            let mut tasks = app.world_mut().query::<&SnapshotVoxelTask>();
            if tasks.iter(app.world()).count() == 0 {
                drained = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(drained, "the snapshot voxel task never finished");
        app.update(); // one more frame: anything it queued has been applied or dropped
        assert!(!app.world().entity(e).contains::<PendingSnapshotVoxels>());
        assert_eq!(app.world().entity(e).get::<VoxelData>(), Some(&VoxelData::new(&edited)));
    }

    #[test]
    fn region_root_sits_at_world_offset() {
        let (mut app, client) = app_shell();
        let (_send, update_recv) = crossbeam::channel::unbounded();
        let region_id = game::RegionCoords::new(1, -2);
        let rb = Rollback::new(None);
        client
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv))
            .unwrap();
        app.update();
        let root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();
        let tf = app.world().entity(root).get::<Transform>().unwrap();
        assert_eq!(tf.translation, Vec3::new(game::REGION_SIZE, 0.0, -2.0 * game::REGION_SIZE));
    }

    #[test]
    fn debug_lines_populate_replace_and_teardown() {
        let (mut app, send) = app_shell();
        let id = RegionCoords::new(0, 0);
        let line = game::DebugLine {
            a: [0.0, 0.0, 0.0],
            b: [1.0, 0.0, 0.0],
            color: [30.0, 1.0, 0.4, 1.0],
        };
        send.send(ClientUpdateEvent::DebugLines(id, vec![line])).unwrap();
        app.update();
        let lines = &app.world().resource::<crate::renderer::collider_debug::DebugColliderLines>().0;
        assert_eq!(lines.get(&id).map(|v| v.len()), Some(1));

        // Replace-on-receipt: an empty buffer overwrites, not appends.
        send.send(ClientUpdateEvent::DebugLines(id, vec![])).unwrap();
        app.update();
        let lines = &app.world().resource::<crate::renderer::collider_debug::DebugColliderLines>().0;
        assert_eq!(lines.get(&id).map(|v| v.len()), Some(0));

        // RemoveRegion drops the entry entirely.
        send.send(ClientUpdateEvent::RemoveRegion(id)).unwrap();
        app.update();
        let lines = &app.world().resource::<crate::renderer::collider_debug::DebugColliderLines>().0;
        assert!(lines.get(&id).is_none());
    }

    #[test]
    fn remove_region_tears_down_root_maps_and_receiver() {
        let (mut app, client, _updates, region_id) = test_app();
        app.update();
        let root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();

        client.send(ClientUpdateEvent::RemoveRegion(region_id)).unwrap();
        app.update();

        assert!(app.world().get_entity(root).is_err(), "root despawned (with children)");
        assert!(!app.world().resource::<RegionRoots>().0.contains_key(&region_id));
        assert!(!app.world().resource::<Regions>().0.contains_key(&region_id));
        assert!(app.world().resource::<SimEntityMap>().0.keys().all(|(r, _)| *r != region_id));
    }

    #[test]
    fn new_region_for_loaded_region_replaces_it() {
        let (mut app, client, _updates, region_id) = test_app();
        app.update();
        let first_root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();

        let (_send2, update_recv2) = crossbeam::channel::unbounded();
        let rb = Rollback::new(None);
        client
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv2))
            .unwrap();
        app.update();

        let second_root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();
        assert_ne!(first_root, second_root, "old root replaced");
        assert!(app.world().get_entity(first_root).is_err(), "old root despawned");
    }

    #[test]
    fn snapshot_spawns_camera_only_for_local_player() {
        let (mut app, client) = app_shell();
        let (_update_send, update_recv) = crossbeam::channel::unbounded();

        // Region snapshot that already contains two players (the join flow).
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        rb.create_player_safe(0);
        rb.create_player_safe(1);
        let data = (*rb.data).clone();
        let k0 = *data.player_entites.get(&0).unwrap();
        let k1 = *data.player_entites.get(&1).unwrap();

        let region_id = RegionCoords::new(0, 0);
        // SetPlayer always precedes NewRegion (PlayerRegion precedes Region).
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_id, data, update_recv)).unwrap();
        app.update();

        let map = app.world().resource::<SimEntityMap>();
        let e0 = *map.0.get(&(region_id, k0)).unwrap();
        let e1 = *map.0.get(&(region_id, k1)).unwrap();
        assert!(app.world().entity(e0).contains::<Camera3d>(), "local player's camera is active");
        assert!(
            !app.world().entity(e1).contains::<Camera3d>(),
            "other player's camera from the snapshot must not be active"
        );
    }

    #[test]
    fn ghost_hidden_only_while_its_source_region_is_loaded() {
        let (mut app, client, updates, region_id) = test_app();
        app.add_systems(Update, dedupe_ghosts);
        app.update();

        let k = key(11);
        let src = RegionCoords::new(1, 0);
        updates.send(GameDataUpdate::new(GameDataTransactionKind::Do, GameDataUpdateKind::CreateEntity(k))).unwrap();
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetGhostSource(k, Some(src)),
        )).unwrap();
        app.update();
        app.update(); // component insert lands a frame after spawn-in-same-drain

        let e = *app.world().resource::<SimEntityMap>().0.get(&(region_id, k)).unwrap();
        // Source region NOT loaded: ghost visible.
        assert_ne!(
            *app.world().entity(e).get::<Visibility>().unwrap(),
            Visibility::Hidden,
            "ghost renders while its source region is absent"
        );

        // Load the source region: the owned copy renders there; hide the ghost.
        let (_s2, recv2) = crossbeam::channel::unbounded();
        let rb2 = Rollback::new(None);
        client.send(ClientUpdateEvent::NewRegion(src, (*rb2.data).clone(), recv2)).unwrap();
        app.update();
        app.update();
        assert_eq!(
            *app.world().entity(e).get::<Visibility>().unwrap(),
            Visibility::Hidden,
            "ghost hides when its source region loads"
        );

        // Unload the source: ghost visible again.
        client.send(ClientUpdateEvent::RemoveRegion(src)).unwrap();
        app.update();
        app.update();
        assert_ne!(*app.world().entity(e).get::<Visibility>().unwrap(), Visibility::Hidden);

        // Upgrade clears the mark: never hidden again.
        updates.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetGhostSource(k, None),
        )).unwrap();
        client.send(ClientUpdateEvent::NewRegion(src, (*Rollback::new(None).data).clone(), {
            let (_s3, r3) = crossbeam::channel::unbounded();
            r3
        })).unwrap();
        app.update();
        app.update();
        assert_ne!(
            *app.world().entity(e).get::<Visibility>().unwrap(),
            Visibility::Hidden,
            "upgraded (owned) entity is never deduped"
        );
    }

    #[test]
    fn only_home_region_camera_is_active_across_regions() {
        use game::ConnectionState;

        let (mut app, client) = app_shell();
        app.add_systems(
            PreUpdate,
            enforce_single_active_camera.after(drain_region_updates),
        );

        // Two loaded regions that BOTH have the local player owning a camera —
        // the transient dual-ownership a boundary handoff produces.
        let region_a = RegionCoords::new(0, 0);
        let region_b = RegionCoords::new(1, 0);

        let mut rb_a = Rollback::new(None);
        rb_a.new_transaction();
        rb_a.create_player_safe(0);
        let data_a = (*rb_a.data).clone();
        let ka = *data_a.player_entites.get(&0).unwrap();

        let mut rb_b = Rollback::new(None);
        rb_b.new_transaction();
        rb_b.create_player_safe(0);
        let data_b = (*rb_b.data).clone();
        let kb = *data_b.player_entites.get(&0).unwrap();

        let (_sa, ra) = crossbeam::channel::unbounded();
        let (_sb, rbr) = crossbeam::channel::unbounded();
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_a, data_a, ra)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_b, data_b, rbr)).unwrap();
        client
            .send(ClientUpdateEvent::HudStatus {
                home_region: region_a,
                viewer_region: region_a,
                connection: ConnectionState::Ready,
            })
            .unwrap();
        app.update();
        app.update();

        let map = app.world().resource::<SimEntityMap>();
        let ea = *map.0.get(&(region_a, ka)).unwrap();
        let eb = *map.0.get(&(region_b, kb)).unwrap();

        assert!(
            app.world().entity(ea).get::<Camera>().unwrap().is_active,
            "home-region camera must be the active one"
        );
        assert!(
            !app.world().entity(eb).get::<Camera>().unwrap().is_active,
            "non-home-region camera must be inactive"
        );
        assert!(
            app.world().entity(ea).contains::<bevy::ui::IsDefaultUiCamera>(),
            "home-region camera must be the default UI camera"
        );
        assert!(
            !app.world().entity(eb).contains::<bevy::ui::IsDefaultUiCamera>(),
            "non-home-region camera must not keep the default UI camera marker"
        );
    }

    #[test]
    fn active_camera_follows_home_region_flip() {
        use game::ConnectionState;

        let (mut app, client) = app_shell();
        app.add_systems(
            PreUpdate,
            enforce_single_active_camera.after(drain_region_updates),
        );

        let region_a = RegionCoords::new(0, 0);
        let region_b = RegionCoords::new(1, 0);
        let mut rb_a = Rollback::new(None);
        rb_a.new_transaction();
        rb_a.create_player_safe(0);
        let data_a = (*rb_a.data).clone();
        let ka = *data_a.player_entites.get(&0).unwrap();
        let mut rb_b = Rollback::new(None);
        rb_b.new_transaction();
        rb_b.create_player_safe(0);
        let data_b = (*rb_b.data).clone();
        let kb = *data_b.player_entites.get(&0).unwrap();

        let (_sa, ra) = crossbeam::channel::unbounded();
        let (_sb, rbr) = crossbeam::channel::unbounded();
        client.send(ClientUpdateEvent::SetPlayer(0)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_a, data_a, ra)).unwrap();
        client.send(ClientUpdateEvent::NewRegion(region_b, data_b, rbr)).unwrap();
        let hud = |home| ClientUpdateEvent::HudStatus {
            home_region: home,
            viewer_region: home,
            connection: ConnectionState::Ready,
        };
        client.send(hud(region_a)).unwrap();
        app.update();
        app.update();

        let (ea, eb) = {
            let map = app.world().resource::<SimEntityMap>();
            (*map.0.get(&(region_a, ka)).unwrap(), *map.0.get(&(region_b, kb)).unwrap())
        };
        assert!(app.world().entity(ea).get::<Camera>().unwrap().is_active);
        assert!(!app.world().entity(eb).get::<Camera>().unwrap().is_active);

        // Cross into region_b: home flips. The region_b camera — whose
        // `Camera3d` was never removed, only deactivated — must reactivate,
        // and region_a's must go inactive. No AddCameraComponent is replayed.
        client.send(hud(region_b)).unwrap();
        app.update();

        assert!(
            !app.world().entity(ea).get::<Camera>().unwrap().is_active,
            "old home camera deactivates after the flip"
        );
        assert!(
            app.world().entity(eb).get::<Camera>().unwrap().is_active,
            "new home camera reactivates after the flip"
        );
        assert!(app.world().entity(eb).contains::<bevy::ui::IsDefaultUiCamera>());
        assert!(!app.world().entity(ea).contains::<bevy::ui::IsDefaultUiCamera>());
    }

    #[test]
    fn drain_client_updates_writes_hud_status() {
        use game::ConnectionState;

        let (client, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(ClientUpdates(client_recv))
            .init_resource::<LocalPlayer>()
            .init_resource::<Regions>()
            .init_resource::<RegionRoots>()
            .init_resource::<SimEntityMap>()
            .init_resource::<HudStatus>()
            .init_resource::<crate::renderer::ui_bridge::UiUpdates>()
            .init_resource::<crate::renderer::collider_debug::DebugColliderLines>()
            .init_resource::<lod_rings::LodRings>()
            .add_systems(Update, drain_client_updates);

        client
            .send(ClientUpdateEvent::HudStatus {
                home_region: RegionCoords::new(2, -1),
                viewer_region: RegionCoords::new(2, 0),
                connection: ConnectionState::Ready,
            })
            .unwrap();

        app.update();

        let status = app.world().resource::<HudStatus>();
        assert_eq!(status.home_region, Some(RegionCoords::new(2, -1)));
        assert_eq!(status.viewer_region, Some(RegionCoords::new(2, 0)));
        assert_eq!(status.connection, ConnectionState::Ready);
    }
}

#[cfg(test)]
mod voxel_data_tests {
    use super::VoxelData;
    use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

    fn dense(f: impl Fn(usize) -> u16) -> Vec<Voxel> {
        (0..CHUNK_VOXEL_COUNT).map(|i| Voxel::new(BlockId(f(i)))).collect()
    }

    /// The contract the mesher depends on: decode(new(v)) == v, bit for bit.
    #[test]
    fn round_trips_exactly() {
        for case in [
            dense(|_| 0),                                   // all air: 1 run
            dense(|_| 7),                                   // uniform solid
            dense(|i| (i % 2) as u16),                      // worst case: no runs
            dense(|i| ((i / 16) % 3) as u16),               // block-aligned runs
            dense(|i| if i == 12345 { 4 } else { 0 }),      // single chiselled voxel
        ] {
            let encoded = VoxelData::new(&case);
            assert_eq!(encoded.decode(), case, "RLE round trip must be exact");
        }
    }

    /// Uniform chunks — the overwhelmingly common case — must collapse to one
    /// run. This is where the ~183 MB/region saving actually comes from.
    #[test]
    fn uniform_chunk_is_one_run() {
        assert_eq!(VoxelData::new(dense(|_| 0)).runs(), 1);
        assert_eq!(VoxelData::new(dense(|_| 9)).runs(), 1);
    }

    /// A chunk of 8 uniform 16³ blocks must never need more than 32 runs per
    /// row of 32 voxels — runs are >= 16 by construction, so the encoding can
    /// not silently degenerate to the dense size for ordinary terrain.
    #[test]
    fn block_aligned_chunk_stays_compact() {
        // Two materials split at the x=16 block boundary.
        let v = dense(|i| if i % 32 < 16 { 1 } else { 2 });
        let runs = VoxelData::new(&v).runs();
        assert_eq!(runs, 2048, "one run per 16-voxel half-row");
        assert!(runs * 4 < CHUNK_VOXEL_COUNT * 2, "still smaller than the dense form");
    }

    /// edit() must decode, apply, and re-encode — the path chisels take.
    #[test]
    fn edit_round_trips() {
        let mut d = VoxelData::new(dense(|_| 0));
        assert_eq!(d.runs(), 1);
        d.edit(|v| v[100] = Voxel::new(BlockId(3)));
        let out = d.decode();
        assert_eq!(out[100], Voxel::new(BlockId(3)));
        assert_eq!(out[99], Voxel::new(BlockId(0)));
        assert_eq!(out.len(), CHUNK_VOXEL_COUNT);
        assert_eq!(d.runs(), 3, "air | one voxel | air");
    }

    #[test]
    fn sorted_index_visit_reads_rle_without_dense_decode() {
        let dense = dense(|i| (i % 11) as u16);
        let data = VoxelData::new(&dense);
        let indices = [0usize, 1, 31, 32, 1024, 8191, CHUNK_VOXEL_COUNT - 1];
        let mut got = Vec::new();

        data.for_each_index_sorted(indices, |index, voxel| got.push((index, voxel)));

        let expected = indices.into_iter().map(|i| (i, dense[i])).collect::<Vec<_>>();
        assert_eq!(got, expected);
    }
}
