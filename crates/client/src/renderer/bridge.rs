use std::collections::BTreeMap;

use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool};
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use crossbeam::channel::Receiver;
use game::{ClientUpdateEvent, GameData, GameDataUpdate, GameDataUpdateKind, RegionId};
use game::{EntityKey, Voxel};

use super::convert::{iso_to_transform, perspective_to_projection};
use super::hud::HudStatus;
use super::meshing::lod;
use super::{ClientUpdates, LocalPlayer};

#[derive(Resource, Default)]
pub struct Regions(pub BTreeMap<RegionId, Receiver<GameDataUpdate>>);

#[derive(Resource, Default)]
pub struct RegionRoots(pub BTreeMap<RegionId, Entity>);

/// Roots for read-only "far" regions: visual-only chunk trees with no
/// physics/collider, rendered from a `FarSnapshot`. Mutually exclusive with
/// `RegionRoots` for a given id — a region is either simulated (near) or
/// terrain-only (far), never both.
#[derive(Resource, Default)]
pub struct FarRegionRoots(pub BTreeMap<RegionId, Entity>);

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
        // Look lives in the 50Hz sim; the render camera SNAPS its rotation
        // (rot_smoothing = 1.0) so there's no slerp-arc tilt during turns,
        // but LERPS its position so movement glides instead of stepping.
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

#[derive(Component)]
pub struct VoxelData(pub Vec<Voxel>);

/// Mirror of the sim's `EntityKind` for this entity. Consumed by the avatar
/// system to attach a renderable mesh.
#[derive(Component, Clone, Copy)]
pub struct SimKind(pub game::EntityKind);

/// Marks a bevy entity as a ghost mirror sourced from another region. Used
/// only for render dedupe: when the source region is also loaded locally,
/// the owned copy renders and the ghost hides (sim state keeps both).
#[derive(Component, Clone, Copy)]
pub struct GhostSource(pub RegionId);

pub fn drain_client_updates(
    mut commands: Commands,
    updates: Res<ClientUpdates>,
    mut player: ResMut<LocalPlayer>,
    mut regions: ResMut<Regions>,
    mut roots: ResMut<RegionRoots>,
    mut far_roots: ResMut<FarRegionRoots>,
    mut map: ResMut<SimEntityMap>,
    mut hud_status: ResMut<HudStatus>,
    mut ui_updates: ResMut<crate::renderer::ui_bridge::UiUpdates>,
    mut debug_lines: ResMut<crate::renderer::collider_debug::DebugColliderLines>,
) {
    while let Ok(event) = updates.0.try_recv() {
        match event {
            ClientUpdateEvent::NewRegion(id, data, receiver) => {
                if roots.0.contains_key(&id) {
                    // Replace-on-re-receipt: resubscribe/crash-respawn resync.
                    remove_region(&mut commands, &mut regions, &mut roots, &mut map, id);
                }
                // Near (simulated) and far (visual-only) roots never coexist.
                remove_far_region(&mut commands, &mut far_roots, id);
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
                spawn_region_snapshot(&mut commands, root, id, &data, &mut map, player.0);
                info!("bridge: region {:?} loaded", id);
            }
            ClientUpdateEvent::SetPlayer(client_id) => player.0 = Some(client_id),
            ClientUpdateEvent::RemoveRegion(id) => {
                remove_region(&mut commands, &mut regions, &mut roots, &mut map, id);
                debug_lines.0.remove(&id);
                info!("bridge: region {:?} removed", id);
            }
            ClientUpdateEvent::NewFarRegion(id, snap) => {
                // Replace-on-re-receipt (refresh) and mutual exclusion: a far
                // region never coexists with its near (simulated) root.
                remove_far_region(&mut commands, &mut far_roots, id);
                if roots.0.contains_key(&id) {
                    remove_region(&mut commands, &mut regions, &mut roots, &mut map, id);
                    debug_lines.0.remove(&id);
                }
                spawn_far_region(&mut commands, &mut far_roots, id, &snap);
                info!("bridge: far region {:?} loaded", id);
            }
            ClientUpdateEvent::RemoveFarRegion(id) => {
                remove_far_region(&mut commands, &mut far_roots, id);
                info!("bridge: far region {:?} removed", id);
            }
            ClientUpdateEvent::LodData(..) | ClientUpdateEvent::RemoveLod(_) => {} // Task 11
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

/// Build the render tree for a read-only far region: a root Transform at the
/// region's world offset, plus a `FarLodBuild` task that (in the background)
/// groups the snapshot's chunks into 2×2×2 clusters and downsamples each into
/// a factor-2 LOD grid (`lod::build_far_lod_cells`). `finish_far_lod_builds`
/// polls the task and spawns the LOD children once it resolves. No
/// rigidbody, no collider, and — as of this cutover — no full-res `VoxelData`
/// children: far regions render coarse from the start.
fn spawn_far_region(
    commands: &mut Commands,
    roots: &mut FarRegionRoots,
    id: RegionId,
    snap: &game::FarSnapshot,
) {
    let offset = id.world_offset();
    let root = commands
        .spawn((
            Transform::from_translation(Vec3::new(offset[0], offset[1], offset[2])),
            Visibility::default(),
            Name::new(format!("far region {:?}", id)),
        ))
        .id();

    let chunks = snap.chunks.clone();
    let pool = AsyncComputeTaskPool::get();
    let task = pool.spawn(async move { lod::build_far_lod_cells(chunks) });
    commands.entity(root).insert(lod::FarLodBuild(task));

    roots.0.insert(id, root);
}

/// Poll in-flight `FarLodBuild` tasks (one per far region root, inserted by
/// `spawn_far_region`) and spawn the resulting LOD children: local
/// translation `(group.x*64, group.y*64 - 2, group.z*64)` (64 = 32 cells ×
/// factor 2; `-2` = the y-bias compensation for factor 2, per the spawner
/// contract in `lod.rs`), `scale: splat(2.0)`, `LodChunk { factor: 2 }`,
/// `LodVoxelData`, and `LodNeighbors` wired to any sibling group landed in
/// the same batch. A far region's whole terrain resolves in one task, so
/// every possible same-region neighbor is already in `spawned` by the time
/// wiring runs — no cross-batch or cross-region wiring is needed here.
pub fn finish_far_lod_builds(mut commands: Commands, mut tasks: Query<(Entity, &mut lod::FarLodBuild)>) {
    for (root, mut build) in &mut tasks {
        let Some(cells) = block_on(future::poll_once(&mut build.0)) else { continue };
        commands.entity(root).remove::<lod::FarLodBuild>();

        let mut spawned: BTreeMap<[i32; 3], Entity> = BTreeMap::new();
        for lod::LodCell { group, grid } in cells {
            let translation = Vec3::new(
                (group[0] * 64) as f32,
                (group[1] * 64 - 2) as f32,
                (group[2] * 64) as f32,
            );
            let child = commands
                .spawn((
                    Transform { translation, scale: Vec3::splat(2.0), ..default() },
                    Visibility::default(),
                    lod::LodChunk { factor: 2 },
                    lod::LodVoxelData(grid),
                    ChildOf(root),
                ))
                .id();
            spawned.insert(group, child);
        }

        for (&g, &e) in &spawned {
            let mut neighbors = BTreeMap::new();
            for dx in -1..=1i32 {
                for dy in -1..=1i32 {
                    for dz in -1..=1i32 {
                        if dx == 0 && dy == 0 && dz == 0 {
                            continue;
                        }
                        if let Some(&ne) = spawned.get(&[g[0] + dx, g[1] + dy, g[2] + dz]) {
                            neighbors.insert([dx, dy, dz], ne);
                        }
                    }
                }
            }
            if !neighbors.is_empty() {
                commands.entity(e).insert(lod::LodNeighbors(neighbors));
            }
        }
    }
}

/// Despawn a far region's render tree.
fn remove_far_region(commands: &mut Commands, roots: &mut FarRegionRoots, id: RegionId) {
    if let Some(root) = roots.0.remove(&id) {
        commands.entity(root).despawn();
    }
}

/// Port of the old `TrueWorld::new` snapshot walk.
fn spawn_region_snapshot(
    commands: &mut Commands,
    root: Entity,
    region: RegionId,
    data: &GameData,
    map: &mut SimEntityMap,
    local_player: Option<game::ClientId>,
) {
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
        if let Some(chunk) = data.ecs.chunk.try_get(key) {
            e.insert(VoxelData(game::derive_voxels(&chunk.blocks, &chunk.chisel)));
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
                        commands.entity(e).insert(VoxelData(voxels));
                    } else {
                        warn!("bridge: SetVoxelComponent for unmapped {:?}", key);
                    }
                }
                GameDataUpdateKind::SetVoxelComponent(key, None) => {
                    if let Some(&e) = map.0.get(&(region, key)) {
                        commands.entity(e).remove::<VoxelData>();
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
            .init_resource::<FarRegionRoots>()
            .init_resource::<SimEntityMap>()
            .init_resource::<HudStatus>()
            .init_resource::<crate::renderer::ui_bridge::UiUpdates>()
            .init_resource::<crate::renderer::collider_debug::DebugColliderLines>()
            .add_systems(PreUpdate, (drain_client_updates, drain_region_updates).chain());
        (app, client_send)
    }

    /// Like `app_shell`, but also wired with the mesh pipeline (asset plugin
    /// + queue/LOD systems) so a far region's `FarLodBuild` task can settle
    /// all the way to a meshed `LodVoxelData` child — needed only by the
    /// far-region LOD tests below, which is why it's split from `app_shell`
    /// rather than adding this weight to every bridge test.
    fn far_lod_app_shell() -> (App, crossbeam::channel::Sender<ClientUpdateEvent>) {
        use bevy::mesh::Mesh;
        use crate::renderer::meshing::queue::{ChunkGrid, FrameUploads, MeshBudgets, RemeshQueue};
        use crate::renderer::meshing::visibility::CullingConfig;
        use crate::renderer::block_textures::BlockTextureLayers;

        let (client_send, client_recv) = crossbeam::channel::unbounded();
        let (game_send, _game_recv) = crossbeam::channel::unbounded();
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .insert_resource(ClientUpdates(client_recv))
            .insert_resource(GameEvents(game_send))
            .init_resource::<LocalPlayer>()
            .init_resource::<Regions>()
            .init_resource::<RegionRoots>()
            .init_resource::<FarRegionRoots>()
            .init_resource::<SimEntityMap>()
            .init_resource::<HudStatus>()
            .init_resource::<crate::renderer::ui_bridge::UiUpdates>()
            .init_resource::<crate::renderer::collider_debug::DebugColliderLines>()
            .init_resource::<ChunkGrid>()
            .init_resource::<RemeshQueue>()
            .init_resource::<FrameUploads>()
            .init_resource::<MeshBudgets>()
            .init_resource::<BlockTextureLayers>()
            .init_resource::<CullingConfig>()
            .add_systems(
                PreUpdate,
                (drain_client_updates, finish_far_lod_builds, drain_region_updates).chain(),
            )
            .add_systems(
                Update,
                (
                    crate::renderer::meshing::queue::mark_remesh_dirty,
                    crate::renderer::meshing::queue::prune_chunk_grid,
                    crate::renderer::meshing::queue::spawn_mesh_tasks,
                    crate::renderer::meshing::apply_meshed_chunks,
                    lod::spawn_lod_mesh_tasks,
                    lod::apply_lod_meshes,
                )
                    .chain(),
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
    fn far_region_roots_at_offset_with_meshable_chunks() {
        use game::{Chunk, ChunkCoords, FarSnapshot, RegionCoords};
        use crate::renderer::meshing::queue::tests::wait_until;

        let (mut app, client) = far_lod_app_shell();
        let id = RegionCoords::new(1, -2);
        let snap = FarSnapshot { chunks: vec![(ChunkCoords::new(0, 0, 0), Chunk::flat_floor(8))] };
        client.send(ClientUpdateEvent::NewFarRegion(id, snap)).unwrap();
        app.update();

        // Root exists at the region's world offset (REGION_SIZE, 0, -2·REGION_SIZE).
        let roots = app.world().resource::<FarRegionRoots>();
        let root = *roots.0.get(&id).expect("far root");
        let tf = app.world().get::<Transform>(root).unwrap();
        assert_eq!(tf.translation, Vec3::new(game::REGION_SIZE, 0.0, -2.0 * game::REGION_SIZE));

        // Async LOD path taken: either the build task is still in flight, or
        // (if this frame's tiny single-chunk build already raced ahead of
        // this assertion) it already resolved into an LOD child — either
        // way, never a full-res VoxelData child (mirrors the same
        // task-slot-or-already-attached idiom `in_flight_budget_is_respected...`
        // uses in queue.rs for the same kind of race).
        assert!(
            app.world().entity(root).contains::<lod::FarLodBuild>()
                || app.world_mut().query::<&lod::LodVoxelData>().iter(app.world()).next().is_some(),
            "far root takes the async LOD build path"
        );
        assert!(
            app.world_mut().query::<&VoxelData>().iter(app.world()).next().is_none(),
            "far regions never spawn full-res VoxelData children"
        );

        assert!(
            wait_until(&mut app, |app| {
                app.world_mut().query::<&lod::LodVoxelData>().iter(app.world()).next().is_some()
            }),
            "far region settles into an LOD child carrying LodVoxelData"
        );
    }

    #[test]
    fn remove_far_region_despawns_root() {
        use game::{Chunk, ChunkCoords, FarSnapshot, RegionCoords};
        let (mut app, client) = app_shell();
        let id = RegionCoords::new(0, 0);
        let snap = FarSnapshot { chunks: vec![(ChunkCoords::new(0, 0, 0), Chunk::flat_floor(8))] };
        client.send(ClientUpdateEvent::NewFarRegion(id, snap)).unwrap();
        app.update();
        client.send(ClientUpdateEvent::RemoveFarRegion(id)).unwrap();
        app.update();
        assert!(app.world().resource::<FarRegionRoots>().0.get(&id).is_none());
    }

    /// Shared body for the despawn-mid-build regression: send `NewFarRegion`,
    /// let `frames_before_remove` updates run (0 = same-frame despawn, both
    /// events drained by one `drain_client_updates` call; >0 = the root/task
    /// exist for a real frame or more first), then `RemoveFarRegion`, then
    /// settle. The root's despawn is only safe *today* because `.chain()`
    /// inserts an `ApplyDeferred` between `drain_client_updates` and
    /// `finish_far_lod_builds` — the despawn command from the first system
    /// lands before the second ever gets a chance to poll (or, once resolved,
    /// spawn children under) an entity that no longer exists. This test pins
    /// that property instead of leaving it an unlocked scheduling accident.
    fn assert_far_region_despawn_mid_build_is_clean(frames_before_remove: usize) {
        use game::{Chunk, ChunkCoords, FarSnapshot, RegionCoords};

        let (mut app, client) = far_lod_app_shell();
        let id = RegionCoords::new(0, 0);
        let snap = FarSnapshot { chunks: vec![(ChunkCoords::new(0, 0, 0), Chunk::flat_floor(8))] };
        client.send(ClientUpdateEvent::NewFarRegion(id, snap)).unwrap();
        for _ in 0..frames_before_remove {
            app.update();
        }
        client.send(ClientUpdateEvent::RemoveFarRegion(id)).unwrap();

        // Settle whatever was in flight (build task, pending children) —
        // the assertions below must hold no matter how the removal raced
        // the background build.
        for _ in 0..10 {
            app.update();
        }

        assert!(
            app.world().resource::<FarRegionRoots>().0.get(&id).is_none(),
            "root removed after RemoveFarRegion (frames_before_remove={frames_before_remove})"
        );
        assert_eq!(
            app.world_mut().query::<&lod::LodVoxelData>().iter(app.world()).count(),
            0,
            "no orphaned LodVoxelData entities survive a despawn mid-build \
             (frames_before_remove={frames_before_remove})"
        );
        assert_eq!(
            app.world_mut().query::<&lod::LodChunk>().iter(app.world()).count(),
            0,
            "no orphaned LodChunk entities survive a despawn mid-build \
             (frames_before_remove={frames_before_remove})"
        );
    }

    #[test]
    fn despawn_same_frame_as_spawn_leaves_no_orphaned_lod_entities() {
        assert_far_region_despawn_mid_build_is_clean(0);
    }

    #[test]
    fn despawn_one_frame_after_spawn_leaves_no_orphaned_lod_entities() {
        assert_far_region_despawn_mid_build_is_clean(1);
    }

    #[test]
    fn refresh_while_first_build_in_flight_settles_to_one_roots_worth_of_lod_children() {
        // NewFarRegion re-receipt (refresh) while the first build is still
        // in flight: `spawn_far_region` tears the first root (and its
        // FarLodBuild) down before spawning the second, in the same
        // `drain_client_updates` call — the surviving build must settle to
        // exactly one root's worth of LOD children, never a duplicated set
        // from the torn-down first build.
        use game::{Chunk, ChunkCoords, FarSnapshot, RegionCoords};
        use crate::renderer::meshing::queue::tests::wait_until;

        let (mut app, client) = far_lod_app_shell();
        let id = RegionCoords::new(0, 0);
        let snap = FarSnapshot { chunks: vec![(ChunkCoords::new(0, 0, 0), Chunk::flat_floor(8))] };
        client.send(ClientUpdateEvent::NewFarRegion(id, snap.clone())).unwrap();
        client.send(ClientUpdateEvent::NewFarRegion(id, snap)).unwrap();
        app.update();

        assert_eq!(
            app.world().resource::<FarRegionRoots>().0.len(),
            1,
            "re-receipt leaves exactly one far root for this region"
        );
        let root = *app.world().resource::<FarRegionRoots>().0.get(&id).unwrap();

        assert!(
            wait_until(&mut app, |app| {
                app.world_mut().query::<&lod::LodVoxelData>().iter(app.world()).next().is_some()
            }),
            "the surviving build settles into LOD children"
        );

        let lod_children: Vec<Entity> = app
            .world_mut()
            .query_filtered::<(Entity, &ChildOf), With<lod::LodVoxelData>>()
            .iter(app.world())
            .filter(|(_, c)| c.parent() == root)
            .map(|(e, _)| e)
            .collect();
        assert_eq!(
            lod_children.len(),
            1,
            "exactly one LOD child for the single-chunk snapshot, no duplicates"
        );

        let total_lod = app.world_mut().query::<&lod::LodVoxelData>().iter(app.world()).count();
        assert_eq!(
            total_lod,
            lod_children.len(),
            "no orphaned LOD children left over from the torn-down first build"
        );
    }

    #[test]
    fn far_snapshot_settles_to_lod_children_never_entering_chunk_grid() {
        // A flat floor spanning a full 2×2×2 chunk group: one LOD child
        // should mesh, and the old full-res far path (VoxelData children
        // keying into ChunkGrid) must be provably gone. A full-res chunk
        // spawned elsewhere (unrelated to the far region, per Task 3's
        // `lod_chunks_mesh_and_scale_without_entering_the_grid` pattern in
        // lod.rs) makes the ChunkGrid non-vacuously populated, so "LOD
        // children are absent from it" is an actual check, not a vacuous one.
        use game::{Block, BlockId, Chunk, ChunkCoords, FarSnapshot, RegionCoords};
        use crate::renderer::meshing::queue::tests::{spawn_chunk, wait_until};
        use crate::renderer::meshing::queue::ChunkGrid;

        let (mut app, client) = far_lod_app_shell();
        let id = RegionCoords::new(0, 0);

        let keyed_full_res = spawn_chunk(
            &mut app,
            Vec3::new(999.0, 0.0, 0.0),
            Vec3::ZERO,
            vec![Voxel::new(BlockId(1)); game::CHUNK_VOXEL_COUNT],
        );

        let solid = Chunk::uniform(Block::new(BlockId(1)));
        let mut chunks = Vec::new();
        for x in 0..2usize {
            for y in 0..2i32 {
                for z in 0..2usize {
                    chunks.push((ChunkCoords::new(x, y, z), solid.clone()));
                }
            }
        }
        client
            .send(ClientUpdateEvent::NewFarRegion(id, FarSnapshot { chunks }))
            .unwrap();
        app.update();

        assert!(
            wait_until(&mut app, |app| {
                app.world_mut()
                    .query::<(&lod::LodVoxelData, &Mesh3d)>()
                    .iter(app.world())
                    .next()
                    .is_some()
            }),
            "the 2x2x2 chunk group settles into a meshed LOD child"
        );

        assert!(
            app.world().resource::<ChunkGrid>().0.values().any(|&v| v == keyed_full_res),
            "sanity: the unrelated full-res chunk keys into the grid (non-vacuous)"
        );
        let lod_entities: std::collections::BTreeSet<Entity> = app
            .world_mut()
            .query_filtered::<Entity, With<lod::LodVoxelData>>()
            .iter(app.world())
            .collect();
        assert!(!lod_entities.is_empty(), "sanity: at least one LOD child exists");
        assert!(
            app.world().resource::<ChunkGrid>().0.values().all(|v| !lod_entities.contains(v)),
            "LOD children never key into the world-keyed ChunkGrid"
        );

        let voxel_data_entities: Vec<Entity> = app
            .world_mut()
            .query::<(Entity, &VoxelData)>()
            .iter(app.world())
            .map(|(e, _)| e)
            .collect();
        assert_eq!(
            voxel_data_entities,
            vec![keyed_full_res],
            "the only VoxelData entity is the unrelated sanity chunk; the far region spawned none"
        );
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
            .init_resource::<FarRegionRoots>()
            .init_resource::<SimEntityMap>()
            .init_resource::<HudStatus>()
            .init_resource::<crate::renderer::ui_bridge::UiUpdates>()
            .init_resource::<crate::renderer::collider_debug::DebugColliderLines>()
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
