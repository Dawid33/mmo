use std::collections::{BTreeMap, BTreeSet};

use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};

use super::brick::{FallbackReason, PreparedBrickRoot, RootHandle};
use super::residency::{
    FarMaintenanceOutcome, FarRefreshAdmission, FarResolvedUpload, FarStageOutcome, FarVoxelLimits,
    FarVoxelResidency, NeighborOffset, ResidentFarCell,
};
use crate::renderer::config::{FarRenderBackend, TerrainRenderConfig};
use crate::renderer::meshing::lod::{
    far_task_capacity, LodBuildQueue, LodChunk, LodMeshTask, LodRasterReady, LodVoxelData,
};
use crate::renderer::meshing::queue::MeshBudgets;
#[cfg(test)]
use crate::renderer::meshing::MeshingTask;
use crate::renderer::voxel_pull::ChunkQuads;

#[derive(Component, Clone, Copy, Debug, Default)]
pub struct FarVoxelReady;

#[derive(Component, Clone, Copy, Debug, Default)]
pub struct FarRasterFallback;

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FarVoxelResident {
    pub cell_slot: u32,
    pub root: RootHandle,
    pub factor: u32,
}

impl FarVoxelResident {
    fn from_resident(resident: ResidentFarCell, factor: u32) -> Self {
        Self {
            cell_slot: resident.cell_slot,
            root: resident.root,
            factor,
        }
    }
}

#[derive(Component)]
pub struct FarBrickBuildTask {
    task: Task<Result<PreparedBrickRoot, FallbackReason>>,
    revision: u64,
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FarVoxelUploadPending {
    pub batch_id: u64,
    pub revision: u64,
    pub factor: u32,
}

#[derive(Resource, Default)]
pub struct FarBrickRevisions(BTreeMap<Entity, u64>);

impl FarBrickRevisions {
    fn bump(&mut self, entity: Entity) -> u64 {
        let revision = self.0.entry(entity).or_default();
        *revision = revision.wrapping_add(1).max(1);
        *revision
    }

    pub fn current(&self, entity: Entity) -> u64 {
        self.0.get(&entity).copied().unwrap_or(0)
    }

    fn remove(&mut self, entity: Entity) {
        self.0.remove(&entity);
    }
}

#[derive(Resource, Default)]
pub struct FarDeferredRemovals(BTreeSet<Entity>);

#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct FarPassReadiness {
    pub statically_supported: bool,
    pub pipelines_ready: bool,
    became_ready: bool,
}

impl FarPassReadiness {
    fn usable(self) -> bool {
        self.statically_supported && self.pipelines_ready
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn sync_far_pass_readiness(
    mut commands: Commands,
    channel: Res<super::pass::FarPassStatusChannel>,
    mut readiness: ResMut<FarPassReadiness>,
    ready_cells: Query<Entity, (With<FarVoxelReady>, With<LodVoxelData>)>,
    mut queue: ResMut<LodBuildQueue>,
) {
    let was_usable = readiness.usable();
    let status = channel.snapshot();
    readiness.statically_supported = status.statically_supported;
    readiness.pipelines_ready = status.pipelines_ready;
    readiness.became_ready = !was_usable && readiness.usable();
    if was_usable && !readiness.usable() {
        for entity in &ready_cells {
            commands
                .entity(entity)
                .insert(FarRasterFallback)
                .remove::<FarVoxelReady>();
            queue.enqueue_raster(entity);
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub fn sync_far_pass_readiness(
    mut commands: Commands,
    mut readiness: ResMut<FarPassReadiness>,
    ready_cells: Query<Entity, (With<FarVoxelReady>, With<LodVoxelData>)>,
    mut queue: ResMut<LodBuildQueue>,
) {
    for entity in &ready_cells {
        commands
            .entity(entity)
            .insert(FarRasterFallback)
            .remove::<FarVoxelReady>();
        queue.enqueue_raster(entity);
    }
    readiness.statically_supported = false;
    readiness.pipelines_ready = false;
    readiness.became_ready = false;
}

#[derive(Resource, Clone, Copy, Debug)]
pub struct FarVoxelCapabilities {
    pub limits: Option<FarVoxelLimits>,
}

impl FarVoxelCapabilities {
    pub fn available(limits: FarVoxelLimits) -> Self {
        Self {
            limits: limits.is_usable().then_some(limits),
        }
    }

    pub const fn unavailable() -> Self {
        Self { limits: None }
    }
}

impl Default for FarVoxelCapabilities {
    fn default() -> Self {
        Self::unavailable()
    }
}

/// Brick preparation owns a far-only lane shared with raster LOD tasks. At
/// the default cap of 64, cloned 32^3 grids plus worst-case prepared bricks
/// add less than roughly 9 MiB beyond the independently bounded near lane.
#[allow(clippy::too_many_arguments)]
pub fn queue_far_brick_tasks(
    mut commands: Commands,
    mut queue: ResMut<LodBuildQueue>,
    mut revisions: ResMut<FarBrickRevisions>,
    budgets: Res<MeshBudgets>,
    config: Res<TerrainRenderConfig>,
    capabilities: Res<FarVoxelCapabilities>,
    residency: Res<FarVoxelResidency>,
    readiness: Res<FarPassReadiness>,
    lod_mesh_tasks: Query<(), With<LodMeshTask>>,
    brick_tasks: Query<(), With<FarBrickBuildTask>>,
    changed: Query<Entity, Changed<LodVoxelData>>,
    grids: Query<&LodVoxelData>,
    fallback_cells: Query<Entity, (With<FarRasterFallback>, With<LodVoxelData>)>,
) {
    let brick_mode = config.far == FarRenderBackend::BrickRaymarch;
    for entity in &changed {
        revisions.bump(entity);
        if brick_mode {
            queue.enqueue_brick(entity);
            commands.entity(entity).remove::<LodMeshTask>();
        }
        // Raster mode: leave the queue alone. `spawn_lod_mesh_tasks` (earlier
        // in this chain) just enqueued this cell on the RASTER lane, and
        // `LodBuildQueue::remove` clears both lanes — calling it here dropped
        // every cell beyond the first task batch of each LOD root, for good.
        // The brick lane needs no per-entity cleanup: `retain_brick` below
        // empties it whenever brick mode is off.
    }
    if brick_mode && readiness.became_ready {
        for entity in &fallback_cells {
            queue.enqueue_brick(entity);
        }
    }
    queue.retain_brick(|entity| brick_mode && grids.contains(entity));
    if !brick_mode || queue.brick_pending_count() == 0 {
        return;
    }

    let admission = residency.refresh_admission();
    if matches!(admission, FarRefreshAdmission::Blocked) {
        return;
    }

    if capabilities.limits.is_none() || !readiness.usable() {
        for entity in queue.brick_batch(usize::MAX) {
            queue.remove(entity);
            commands
                .entity(entity)
                .insert(FarRasterFallback)
                .remove::<FarVoxelReady>();
            queue.enqueue_raster(entity);
        }
        return;
    }

    let capacity = far_task_capacity(
        budgets.max_in_flight,
        lod_mesh_tasks.iter().count(),
        brick_tasks.iter().count(),
    );
    if capacity == 0 {
        return;
    }
    let pool = AsyncComputeTaskPool::get();
    let mut spawned = 0;
    let candidate_limit = if matches!(admission, FarRefreshAdmission::Any) {
        capacity
    } else {
        usize::MAX
    };
    for entity in queue.brick_batch(candidate_limit) {
        if spawned == capacity {
            break;
        }
        if matches!(admission, FarRefreshAdmission::FirstLoadsOnly)
            && (residency.resident(entity).is_some() || residency.has_pending_first_load(entity))
        {
            continue;
        }
        if brick_tasks.contains(entity) {
            continue;
        }
        let Ok(voxels) = grids.get(entity) else {
            queue.remove(entity);
            continue;
        };
        // Packed clone; the dense expansion happens in the task.
        let packed = voxels.0.clone();
        let revision = revisions.current(entity);
        queue.remove(entity);
        let task = pool.spawn(async move {
            let voxels = crate::renderer::meshing::lod::decode_lod_grid(&packed);
            PreparedBrickRoot::from_voxels(&voxels)
        });
        commands
            .entity(entity)
            .insert(FarBrickBuildTask { task, revision });
        spawned += 1;
    }
}

#[allow(clippy::too_many_arguments)]
pub fn apply_far_brick_tasks(
    mut commands: Commands,
    mut queue: ResMut<LodBuildQueue>,
    mut residency: ResMut<FarVoxelResidency>,
    revisions: Res<FarBrickRevisions>,
    readiness: Res<FarPassReadiness>,
    mut tasks: Query<(
        Entity,
        &mut FarBrickBuildTask,
        &LodChunk,
        &Transform,
        &ChildOf,
    )>,
    parents: Query<&Transform, Without<LodChunk>>,
) {
    for (entity, mut task, lod, local, parent) in &mut tasks {
        if !readiness.usable() {
            commands
                .entity(entity)
                .remove::<(FarBrickBuildTask, FarVoxelReady)>()
                .insert(FarRasterFallback);
            queue.enqueue_raster(entity);
            continue;
        }
        let Some(result) = block_on(future::poll_once(&mut task.task)) else {
            continue;
        };
        if task.revision != revisions.current(entity) {
            commands.entity(entity).remove::<FarBrickBuildTask>();
            continue;
        }
        let mut entity_commands = commands.entity(entity);
        let Ok(prepared) = result else {
            entity_commands.remove::<FarBrickBuildTask>();
            if residency.resident(entity).is_none() {
                entity_commands.insert(FarRasterFallback);
                queue.enqueue_raster(entity);
            }
            continue;
        };
        let parent_translation = parents
            .get(parent.parent())
            .map_or(Vec3::ZERO, |transform| transform.translation);
        let raster_biased_origin = parent_translation + local.translation;
        match residency.stage_refresh(entity, prepared, raster_biased_origin, lod.factor) {
            FarStageOutcome::Pending { batch_id, .. } => {
                entity_commands
                    .remove::<FarBrickBuildTask>()
                    .insert(FarVoxelUploadPending {
                        batch_id,
                        revision: task.revision,
                        factor: lod.factor,
                    });
            }
            FarStageOutcome::Unchanged { resident } => {
                entity_commands
                    .remove::<FarBrickBuildTask>()
                    .insert((
                        FarVoxelReady,
                        FarVoxelResident::from_resident(resident, lod.factor),
                    ))
                    .remove::<(
                        FarRasterFallback,
                        LodMeshTask,
                        Mesh3d,
                        ChunkQuads,
                        LodRasterReady,
                    )>();
            }
            FarStageOutcome::Busy => {
                entity_commands.remove::<FarBrickBuildTask>();
                queue.enqueue_brick(entity);
            }
            FarStageOutcome::Fallback { kept: Some(_) } => {
                entity_commands.remove::<FarBrickBuildTask>();
                // The previous GPU cell remains fully referenced and visible.
            }
            FarStageOutcome::Fallback { kept: None } => {
                entity_commands
                    .remove::<FarBrickBuildTask>()
                    .insert(FarRasterFallback);
                queue.enqueue_raster(entity);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_far_upload_results(
    mut commands: Commands,
    channel: Res<super::gpu::FarUploadChannel>,
    mut residency: ResMut<FarVoxelResidency>,
    mut revisions: ResMut<FarBrickRevisions>,
    readiness: Res<FarPassReadiness>,
    mut queue: ResMut<LodBuildQueue>,
    pending_components: Query<&FarVoxelUploadPending>,
    mut deferred_removals: ResMut<FarDeferredRemovals>,
) {
    for result in channel.drain_results() {
        let Some(resolved) = residency.resolve_upload(result) else {
            continue;
        };
        for resolved in resolved {
            match resolved {
                FarResolvedUpload::Committed { entity, resident } => {
                    let Ok(pending) = pending_components.get(entity).copied() else {
                        deferred_removals.0.insert(entity);
                        continue;
                    };
                    if pending.batch_id != result.id {
                        continue;
                    }
                    let is_current = pending.revision == revisions.current(entity);
                    let Ok(mut entity_commands) = commands.get_entity(entity) else {
                        deferred_removals.0.insert(entity);
                        continue;
                    };
                    entity_commands
                        .insert(FarVoxelResident::from_resident(resident, pending.factor))
                        .remove::<FarVoxelUploadPending>();
                    if is_current && readiness.usable() {
                        entity_commands.insert(FarVoxelReady).remove::<(
                            FarRasterFallback,
                            LodMeshTask,
                            Mesh3d,
                            ChunkQuads,
                            LodRasterReady,
                        )>();
                    } else if !readiness.usable() {
                        entity_commands
                            .insert(FarRasterFallback)
                            .remove::<FarVoxelReady>();
                        queue.enqueue_raster(entity);
                    } else {
                        entity_commands.insert(FarVoxelReady).remove::<(
                            FarRasterFallback,
                            LodMeshTask,
                            Mesh3d,
                            ChunkQuads,
                            LodRasterReady,
                        )>();
                        queue.enqueue_brick(entity);
                    }
                }
                FarResolvedUpload::Rejected { entity, kept } => {
                    let Ok(pending) = pending_components.get(entity).copied() else {
                        continue;
                    };
                    if pending.batch_id != result.id {
                        continue;
                    }
                    let Ok(mut entity_commands) = commands.get_entity(entity) else {
                        continue;
                    };
                    entity_commands.remove::<FarVoxelUploadPending>();
                    if kept.is_none() {
                        entity_commands.insert(FarRasterFallback);
                        queue.enqueue_raster(entity);
                    }
                }
                FarResolvedUpload::Updated { .. } => {}
                FarResolvedUpload::Removed { entity } => {
                    deferred_removals.0.remove(&entity);
                    revisions.remove(entity);
                }
            }
        }
    }
}

pub fn refresh_far_neighbors(
    mut residency: ResMut<FarVoxelResidency>,
    queue: Res<LodBuildQueue>,
    brick_tasks: Query<Entity, With<FarBrickBuildTask>>,
    cells: Query<(Entity, &LodChunk, &FarVoxelResident), With<LodVoxelData>>,
) {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct CellCoord {
        factor: u32,
        xyz: [i64; 3],
    }

    if matches!(residency.refresh_admission(), FarRefreshAdmission::Blocked) {
        return;
    }

    let first_load_work_waiting = residency.has_unpublished_first_loads()
        || queue
            .brick_entities()
            .any(|entity| residency.resident(entity).is_none())
        || brick_tasks
            .iter()
            .any(|entity| residency.resident(entity).is_none());
    if first_load_work_waiting {
        return;
    }

    let cell_coord = |metadata: &super::residency::GpuFarCell| {
        let span = f64::from(metadata.factor) * 32.0;
        CellCoord {
            factor: metadata.factor,
            xyz: metadata
                .origin
                .map(|component| (f64::from(component) / span).round() as i64),
        }
    };
    let resident = cells
        .iter()
        .filter_map(|(entity, _, cell)| {
            let metadata = residency.cell_metadata(entity)?;
            Some((cell_coord(metadata), cell.cell_slot))
        })
        .collect::<BTreeMap<_, _>>();
    let mut updates = Vec::new();
    for (entity, lod, _) in &cells {
        let Some(mut metadata) = residency.cell_metadata(entity).copied() else {
            continue;
        };
        metadata
            .neighbor_slots
            .fill(super::residency::INVALID_FAR_SLOT);
        let coord = cell_coord(&metadata);
        for dz in -1..=1_i8 {
            for dy in -1..=1_i8 {
                for dx in -1..=1_i8 {
                    let Some(offset) = NeighborOffset::new(dx, dy, dz) else {
                        continue;
                    };
                    let neighbor = CellCoord {
                        factor: lod.factor,
                        xyz: [
                            coord.xyz[0] + i64::from(dx),
                            coord.xyz[1] + i64::from(dy),
                            coord.xyz[2] + i64::from(dz),
                        ],
                    };
                    if let Some(&slot) = resident.get(&neighbor) {
                        metadata.set_neighbor(offset, slot);
                    }
                }
            }
        }
        updates.push((entity, metadata));
    }
    for (entity, metadata) in updates {
        match residency.stage_cell_update(entity, metadata) {
            FarMaintenanceOutcome::Pending { .. }
            | FarMaintenanceOutcome::Unchanged
            | FarMaintenanceOutcome::Missing => {}
            FarMaintenanceOutcome::Busy => break,
        }
    }
}

pub fn remove_far_voxel_cells(
    mut commands: Commands,
    mut removed: RemovedComponents<LodVoxelData>,
    still_alive: Query<(), With<LodVoxelData>>,
    mut residency: ResMut<FarVoxelResidency>,
    mut queue: ResMut<LodBuildQueue>,
    mut deferred: ResMut<FarDeferredRemovals>,
    mut revisions: ResMut<FarBrickRevisions>,
) {
    for entity in removed.read() {
        if still_alive.contains(entity) {
            continue;
        }
        queue.remove(entity);
        deferred.0.insert(entity);
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.remove::<(
                FarVoxelReady,
                FarVoxelResident,
                FarBrickBuildTask,
                FarVoxelUploadPending,
            )>();
        }
    }
    let Some(entity) = deferred.0.first().copied() else {
        return;
    };
    match residency.stage_remove(entity) {
        FarMaintenanceOutcome::Pending { .. } | FarMaintenanceOutcome::Busy => {}
        FarMaintenanceOutcome::Unchanged => unreachable!("removal cannot be unchanged"),
        FarMaintenanceOutcome::Missing => {
            deferred.0.remove(&entity);
            revisions.remove(entity);
        }
    }
}

pub struct FarVoxelPlugin;

impl Plugin for FarVoxelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FarVoxelCapabilities>()
            .init_resource::<FarVoxelResidency>()
            .init_resource::<FarBrickRevisions>()
            .init_resource::<FarDeferredRemovals>()
            .init_resource::<FarPassReadiness>()
            .init_resource::<super::gpu::FarUploadChannel>();
        #[cfg(not(target_arch = "wasm32"))]
        app.init_resource::<super::pass::FarPassStatusChannel>();
    }

    fn finish(&self, app: &mut App) {
        #[cfg(target_arch = "wasm32")]
        let _ = app;
        #[cfg(not(target_arch = "wasm32"))]
        {
            use bevy::render::{renderer::RenderAdapter, renderer::RenderDevice, RenderApp};

            let device_info = app.get_sub_app(RenderApp).map(|render_app| {
                let device = render_app.world().resource::<RenderDevice>();
                let adapter = render_app.world().resource::<RenderAdapter>();
                (
                    device.limits().max_storage_buffer_binding_size as usize,
                    super::pipeline::far_pass_device_capabilities(device, adapter)
                        .supports_far_pass(),
                )
            });
            if let Some((binding_limit, pass_supported)) = device_info {
                let limits = FarVoxelLimits::from_storage_binding_limit(binding_limit);
                let supported = pass_supported && limits.is_usable();
                app.insert_resource(if supported {
                    FarVoxelCapabilities::available(limits)
                } else {
                    FarVoxelCapabilities::unavailable()
                });
                app.insert_resource(FarVoxelResidency::new(limits));
                let status = app
                    .world()
                    .resource::<super::pass::FarPassStatusChannel>()
                    .clone();
                status.publish_static_device_support(supported);
                if supported {
                    if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
                        super::gpu::install_render_resources(render_app);
                        super::pass::install_render_pass(render_app, status);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::prelude::*;
    use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

    use super::*;
    use crate::renderer::block_textures::BlockTextureLayers;
    use crate::renderer::bridge::VoxelData;
    use crate::renderer::config::{FarRenderBackend, NearRenderBackend, TerrainRenderConfig};
    use crate::renderer::far_voxel::residency::{
        FarUploadResult, FarVoxelLimits, FarVoxelResidency, NeighborOffset,
    };
    use crate::renderer::meshing::lod::{LodBuildQueue, LodChunk, LodVoxelData};
    use crate::renderer::meshing::queue::{ChunkGrid, FrameUploads, MeshBudgets, RemeshQueue};

    fn brick_app(limits: FarVoxelLimits) -> App {
        let mut app = App::new();
        let pass_status = crate::renderer::far_voxel::pass::FarPassStatusChannel::default();
        pass_status.publish_static_device_support(true);
        pass_status.publish_render_status(true, 0, 0);
        app.add_plugins(MinimalPlugins)
            .insert_resource(TerrainRenderConfig {
                near: NearRenderBackend::GroupedDirect,
                far: FarRenderBackend::BrickRaymarch,
            })
            .insert_resource(MeshBudgets {
                max_in_flight: 8,
                uploads_per_frame: 8,
            })
            .init_resource::<LodBuildQueue>()
            .init_resource::<crate::renderer::meshing::lod::FarFrameUploads>()
            .init_resource::<BlockTextureLayers>()
            .init_resource::<FarBrickRevisions>()
            .init_resource::<FarDeferredRemovals>()
            .init_resource::<FarPassReadiness>()
            .init_resource::<super::super::gpu::FarUploadChannel>()
            .insert_resource(pass_status)
            .insert_resource(FarVoxelCapabilities::available(limits))
            .insert_resource(FarVoxelResidency::new(limits))
            .add_systems(
                Update,
                (
                    sync_far_pass_readiness,
                    resolve_far_upload_results,
                    crate::renderer::meshing::lod::spawn_lod_mesh_tasks,
                    queue_far_brick_tasks,
                    remove_far_voxel_cells,
                    refresh_far_neighbors,
                    apply_far_brick_tasks,
                    super::super::gpu::publish_far_uploads,
                )
                    .chain(),
            );
        app
    }

    fn grid(material: BlockId) -> Vec<Voxel> {
        let mut grid = vec![Voxel::new(BlockId::AIR); CHUNK_VOXEL_COUNT];
        grid[0] = Voxel::new(material);
        grid
    }

    fn spawn_cell(app: &mut App, local: Vec3, factor: u32, material: BlockId) -> Entity {
        let root = app
            .world_mut()
            .spawn(Transform::from_translation(Vec3::new(1000.0, 32.0, -500.0)))
            .id();
        app.world_mut()
            .spawn((
                Transform::from_translation(local - Vec3::Y * factor as f32),
                LodChunk { factor },
                LodVoxelData::from_dense(&grid(material)),
                ChildOf(root),
            ))
            .id()
    }

    fn wait_until(app: &mut App, mut predicate: impl FnMut(&App) -> bool) -> bool {
        for _ in 0..100 {
            app.update();
            let channel = app
                .world()
                .resource::<super::super::gpu::FarUploadChannel>()
                .clone();
            for batch in channel.drain_batches() {
                channel.send_result(FarUploadResult::ack(batch.id));
            }
            if predicate(app) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    }

    /// Production system order in RASTER mode: `queue_far_brick_tasks` sits
    /// between `spawn_lod_mesh_tasks` and `apply_lod_meshes` (`renderer/mod.rs`).
    fn raster_app() -> App {
        let mut app = brick_app(FarVoxelLimits::for_tests());
        app.add_plugins(bevy::asset::AssetPlugin::default())
            .init_asset::<Mesh>()
            .insert_resource(TerrainRenderConfig {
                near: NearRenderBackend::GroupedDirect,
                far: FarRenderBackend::Raster,
            })
            .add_systems(
                Update,
                crate::renderer::meshing::lod::apply_lod_meshes.after(queue_far_brick_tasks),
            );
        app
    }

    /// Regression (2026-09-19, live): a LOD root arrives as hundreds of cells
    /// in one frame. The raster lane meshes `max_in_flight` of them per batch
    /// and must keep the rest queued. `queue_far_brick_tasks` used to call
    /// `queue.remove()` — which clears BOTH lanes — on every changed cell in
    /// raster mode, in the same frame, so only the first batch of each root
    /// was ever meshed: neighbouring regions rendered as a thin strip.
    #[test]
    fn raster_mode_meshes_every_cell_of_a_root_larger_than_the_task_budget() {
        let mut app = raster_app();
        let budget = app.world().resource::<MeshBudgets>().max_in_flight;
        let cells: Vec<Entity> = (0..budget * 3)
            .map(|i| spawn_cell(&mut app, Vec3::X * 64.0 * i as f32, 2, BlockId(7)))
            .collect();

        let all_ready = |app: &App| {
            cells
                .iter()
                .all(|&cell| app.world().entity(cell).contains::<LodRasterReady>())
        };
        assert!(
            wait_until(&mut app, all_ready),
            "only {} of {} cells were ever meshed",
            cells
                .iter()
                .filter(|&&cell| app.world().entity(cell).contains::<LodRasterReady>())
                .count(),
            cells.len()
        );
        assert_eq!(app.world().resource::<LodBuildQueue>().pending_count(), 0);
    }

    #[test]
    fn first_load_waits_for_render_ack_and_nack_enqueues_cell_raster_fallback() {
        let mut app = brick_app(FarVoxelLimits::for_tests());
        let entity = spawn_cell(&mut app, Vec3::ZERO, 2, BlockId(5));
        for _ in 0..100 {
            app.update();
            if app
                .world()
                .entity(entity)
                .contains::<FarVoxelUploadPending>()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(app
            .world()
            .entity(entity)
            .contains::<FarVoxelUploadPending>());
        assert!(!app.world().entity(entity).contains::<FarVoxelReady>());
        let channel = app
            .world()
            .resource::<super::super::gpu::FarUploadChannel>()
            .clone();
        let batch = channel.drain_batches().pop().unwrap();
        channel.send_result(FarUploadResult::nack(batch.id));
        app.update();

        assert!(!app.world().entity(entity).contains::<FarVoxelReady>());
        assert!(app.world().entity(entity).contains::<FarRasterFallback>());
    }

    #[test]
    fn changed_grid_during_in_flight_build_discards_stale_result_and_builds_latest_revision() {
        let mut app = brick_app(FarVoxelLimits::for_tests());
        app.world_mut().insert_resource(MeshBudgets {
            max_in_flight: 0,
            uploads_per_frame: 8,
        });
        let entity = spawn_cell(&mut app, Vec3::ZERO, 2, BlockId(4));
        app.update();
        let old_revision = app.world().resource::<FarBrickRevisions>().current(entity);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let old_prepared = PreparedBrickRoot::from_voxels(&grid(BlockId(4))).unwrap();
        let controlled = AsyncComputeTaskPool::get().spawn(async move {
            release_rx.recv().unwrap();
            Ok(old_prepared)
        });
        app.world_mut()
            .entity_mut(entity)
            .insert(FarBrickBuildTask {
                task: controlled,
                revision: old_revision,
            });
        app.world_mut().insert_resource(MeshBudgets {
            max_in_flight: 8,
            uploads_per_frame: 8,
        });
        app.world_mut().get_mut::<LodVoxelData>(entity).unwrap()
            .0 = LodVoxelData::from_dense(&grid(BlockId(12))).0;
        app.update();
        assert!(
            app.world()
                .resource::<LodBuildQueue>()
                .brick_pending_count()
                > 0
        );
        release_tx.send(()).unwrap();

        assert!(wait_until(&mut app, |app| {
            app.world().entity(entity).contains::<FarVoxelReady>()
        }));
        let reconstructed = app
            .world()
            .resource::<FarVoxelResidency>()
            .reconstruct_cell(entity)
            .unwrap();
        assert_eq!(reconstructed[0].block, BlockId(12));
    }

    #[test]
    fn brick_mode_builds_off_thread_and_marks_true_origin_ready() {
        let mut app = brick_app(FarVoxelLimits::for_tests());
        let entity = spawn_cell(&mut app, Vec3::new(64.0, 0.0, 96.0), 2, BlockId(3));

        assert!(wait_until(&mut app, |app| {
            app.world().entity(entity).contains::<FarVoxelReady>()
        }));
        let resident = *app
            .world()
            .entity(entity)
            .get::<FarVoxelResident>()
            .unwrap();
        let metadata = app
            .world()
            .resource::<FarVoxelResidency>()
            .cell_metadata(entity)
            .unwrap();
        assert_eq!(resident.factor, 2);
        assert_eq!(metadata.origin, [1064.0, 32.0, -404.0]);
        assert_eq!(app.world().resource::<LodBuildQueue>().pending_count(), 0);
        assert!(!app
            .world()
            .entity(entity)
            .contains::<crate::renderer::meshing::lod::LodMeshTask>());
    }

    #[test]
    fn completed_first_loads_publish_one_batch_and_promote_on_one_ack() {
        let mut app = brick_app(FarVoxelLimits::for_tests());
        app.world_mut().insert_resource(MeshBudgets {
            max_in_flight: 0,
            uploads_per_frame: 8,
        });
        let entities = [
            spawn_cell(&mut app, Vec3::ZERO, 2, BlockId(21)),
            spawn_cell(&mut app, Vec3::X * 64.0, 2, BlockId(22)),
            spawn_cell(&mut app, Vec3::Z * 64.0, 2, BlockId(23)),
        ];
        app.update();
        for (entity, material) in entities.into_iter().zip([21, 22, 23].map(BlockId)) {
            let revision = app.world().resource::<FarBrickRevisions>().current(entity);
            let prepared = PreparedBrickRoot::from_voxels(&grid(material)).unwrap();
            let task = AsyncComputeTaskPool::get().spawn(async move { Ok(prepared) });
            app.world_mut()
                .entity_mut(entity)
                .insert(FarBrickBuildTask { task, revision });
        }
        std::thread::sleep(Duration::from_millis(5));

        app.update();

        let batch_ids = entities.map(|entity| {
            app.world()
                .entity(entity)
                .get::<FarVoxelUploadPending>()
                .expect("every completed first load joins the upload batch")
                .batch_id
        });
        assert!(batch_ids.iter().all(|id| *id == batch_ids[0]));
        let channel = app
            .world()
            .resource::<super::super::gpu::FarUploadChannel>()
            .clone();
        let batches = channel.drain_batches();
        assert_eq!(batches.len(), 1);
        assert_eq!(
            batches[0]
                .uploads
                .iter()
                .filter(|upload| matches!(
                    upload,
                    super::super::residency::FarUpload::WriteCell { .. }
                ))
                .count(),
            entities.len()
        );

        channel.send_result(FarUploadResult::ack(batch_ids[0]));
        app.update();
        assert!(entities
            .iter()
            .all(|entity| app.world().entity(*entity).contains::<FarVoxelReady>()));
    }

    #[test]
    fn partial_first_load_batch_coalesces_until_remaining_work_is_exhausted() {
        let mut app = brick_app(FarVoxelLimits::for_tests());
        app.world_mut().insert_resource(MeshBudgets {
            max_in_flight: 0,
            uploads_per_frame: 8,
        });
        let entities = [
            spawn_cell(&mut app, Vec3::ZERO, 2, BlockId(24)),
            spawn_cell(&mut app, Vec3::X * 64.0, 2, BlockId(25)),
            spawn_cell(&mut app, Vec3::Z * 64.0, 2, BlockId(26)),
        ];
        app.update();

        let mut releases = Vec::new();
        for (entity, material) in entities.into_iter().zip([24, 25, 26].map(BlockId)) {
            app.world_mut()
                .resource_mut::<LodBuildQueue>()
                .remove(entity);
            let revision = app.world().resource::<FarBrickRevisions>().current(entity);
            let prepared = PreparedBrickRoot::from_voxels(&grid(material)).unwrap();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let task = AsyncComputeTaskPool::get().spawn(async move {
                let _ = release_rx.recv();
                Ok(prepared)
            });
            app.world_mut()
                .entity_mut(entity)
                .insert(FarBrickBuildTask { task, revision });
            releases.push(release_tx);
        }
        app.world_mut().insert_resource(MeshBudgets {
            max_in_flight: 4,
            uploads_per_frame: 8,
        });
        releases[0].send(()).unwrap();
        releases[1].send(()).unwrap();
        std::thread::sleep(Duration::from_millis(5));

        app.update();

        let channel = app
            .world()
            .resource::<super::super::gpu::FarUploadChannel>()
            .clone();
        assert!(
            channel.drain_batches().is_empty(),
            "two staged cells stay unpublished while a third build is in flight"
        );

        app.world_mut()
            .get_mut::<LodVoxelData>(entities[0])
            .unwrap()
            .0 = LodVoxelData::from_dense(&grid(BlockId(31))).0;
        app.update();
        assert!(
            app.world()
                .resource::<LodBuildQueue>()
                .brick_entities()
                .any(|entity| entity == entities[0]),
            "the newer revision stays queued behind its staged first load"
        );
        assert!(!app
            .world()
            .entity(entities[0])
            .contains::<FarBrickBuildTask>());
        assert!(channel.drain_batches().is_empty());

        releases[2].send(()).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        app.update();

        let batches = channel.drain_batches();
        assert_eq!(batches.len(), 1);
        assert_eq!(
            batches[0]
                .uploads
                .iter()
                .filter(|upload| matches!(
                    upload,
                    super::super::residency::FarUpload::WriteCell { .. }
                ))
                .count(),
            3
        );
        channel.send_result(FarUploadResult::ack(batches[0].id));
        assert!(wait_until(&mut app, |app| {
            app.world()
                .resource::<FarVoxelResidency>()
                .reconstruct_cell(entities[0])
                .is_some_and(|voxels| voxels[0].block == BlockId(31))
                && app.world().entity(entities[0]).contains::<FarVoxelReady>()
        }));
        assert_eq!(app.world().resource::<LodBuildQueue>().pending_count(), 0);
    }

    #[test]
    fn saturated_near_lane_does_not_prevent_far_brick_spawn() {
        let limits = FarVoxelLimits::for_tests();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TerrainRenderConfig {
                near: NearRenderBackend::GroupedDirect,
                far: FarRenderBackend::BrickRaymarch,
            })
            .insert_resource(MeshBudgets {
                max_in_flight: 1,
                uploads_per_frame: 0,
            })
            .init_resource::<ChunkGrid>()
            .init_resource::<RemeshQueue>()
            .init_resource::<FrameUploads>()
            .init_resource::<LodBuildQueue>()
            .init_resource::<BlockTextureLayers>()
            .init_resource::<FarBrickRevisions>()
            .insert_resource(FarVoxelCapabilities::available(limits))
            .insert_resource(FarPassReadiness {
                statically_supported: true,
                pipelines_ready: true,
                became_ready: false,
            })
            .insert_resource(FarVoxelResidency::new(limits))
            .add_systems(
                Update,
                (
                    crate::renderer::meshing::queue::mark_remesh_dirty,
                    crate::renderer::meshing::queue::spawn_mesh_tasks,
                    queue_far_brick_tasks,
                )
                    .chain(),
            );
        let near = app
            .world_mut()
            .spawn(VoxelData::new(grid(BlockId(27))))
            .id();
        let far = app.world_mut().spawn(LodVoxelData::from_dense(&grid(BlockId(28)))).id();

        app.update();

        assert!(app.world().entity(near).contains::<MeshingTask>());
        assert!(
            app.world().entity(far).contains::<FarBrickBuildTask>(),
            "far owns an independent max_in_flight lane"
        );
    }

    #[test]
    fn published_upload_batch_pauses_brick_spawning_until_resolution() {
        let limits = FarVoxelLimits::for_tests();
        let mut residency = FarVoxelResidency::new(limits);
        let blocker = Entity::from_bits(70);
        let prepared = PreparedBrickRoot::from_voxels(&grid(BlockId(20))).unwrap();
        let FarStageOutcome::Pending { batch_id, .. } =
            residency.stage_refresh(blocker, prepared, Vec3::ZERO, 2)
        else {
            panic!("blocker must stage");
        };
        residency.take_upload_batch().unwrap();
        assert_eq!(residency.refresh_admission(), FarRefreshAdmission::Blocked);

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TerrainRenderConfig {
                near: NearRenderBackend::GroupedDirect,
                far: FarRenderBackend::BrickRaymarch,
            })
            .insert_resource(MeshBudgets {
                max_in_flight: 8,
                uploads_per_frame: 8,
            })
            .init_resource::<LodBuildQueue>()
            .init_resource::<FarBrickRevisions>()
            .insert_resource(FarVoxelCapabilities::available(limits))
            .insert_resource(FarPassReadiness {
                statically_supported: true,
                pipelines_ready: true,
                became_ready: false,
            })
            .insert_resource(residency)
            .add_systems(Update, queue_far_brick_tasks);
        let queued = app.world_mut().spawn(LodVoxelData::from_dense(&grid(BlockId(21)))).id();

        app.update();

        assert_eq!(
            app.world()
                .resource::<LodBuildQueue>()
                .brick_pending_count(),
            1
        );
        assert!(!app.world().entity(queued).contains::<FarBrickBuildTask>());

        app.world_mut()
            .resource_mut::<FarVoxelResidency>()
            .resolve_upload(FarUploadResult::ack(batch_id));
        app.update();
        assert!(app.world().entity(queued).contains::<FarBrickBuildTask>());
    }

    #[test]
    fn unavailable_capability_falls_back_only_the_failed_cell() {
        let mut app = brick_app(FarVoxelLimits::for_tests());
        app.world_mut()
            .insert_resource(FarVoxelCapabilities::unavailable());
        let entity = spawn_cell(&mut app, Vec3::ZERO, 4, BlockId(3));

        app.update();

        assert!(app.world().entity(entity).contains::<FarRasterFallback>());
        assert!(!app.world().entity(entity).contains::<FarVoxelReady>());
        assert_eq!(
            app.world()
                .resource::<LodBuildQueue>()
                .raster_pending_count(),
            1
        );
    }

    #[test]
    fn compiling_pipeline_keeps_raster_then_requeues_fallback_on_ready_transition() {
        let mut app = brick_app(FarVoxelLimits::for_tests());
        let status = app
            .world()
            .resource::<crate::renderer::far_voxel::pass::FarPassStatusChannel>()
            .clone();
        status.publish_render_status(false, 0, 0);
        let entity = spawn_cell(&mut app, Vec3::ZERO, 4, BlockId(7));

        app.update();
        assert!(app.world().entity(entity).contains::<FarRasterFallback>());
        assert!(!app.world().entity(entity).contains::<FarVoxelReady>());

        status.publish_render_status(true, 0, 0);
        app.update();
        assert!(app.world().entity(entity).contains::<FarRasterFallback>());
        assert!(
            app.world()
                .resource::<LodBuildQueue>()
                .brick_pending_count()
                > 0
                || app.world().entity(entity).contains::<FarBrickBuildTask>()
                || app
                    .world()
                    .entity(entity)
                    .contains::<FarVoxelUploadPending>()
        );

        assert!(wait_until(&mut app, |app| {
            app.world().entity(entity).contains::<FarVoxelReady>()
        }));
        assert!(!app.world().entity(entity).contains::<FarRasterFallback>());
        assert!(!app.world().entity(entity).contains::<LodMeshTask>());
    }

    #[test]
    fn losing_pipeline_readiness_demotes_existing_gpu_cells_to_raster() {
        let mut app = brick_app(FarVoxelLimits::for_tests());
        let entity = spawn_cell(&mut app, Vec3::ZERO, 4, BlockId(8));
        assert!(wait_until(&mut app, |app| {
            app.world().entity(entity).contains::<FarVoxelReady>()
        }));
        let status = app
            .world()
            .resource::<crate::renderer::far_voxel::pass::FarPassStatusChannel>()
            .clone();
        status.publish_render_status(false, 0, 0);

        app.update();

        assert!(app.world().entity(entity).contains::<FarRasterFallback>());
        assert!(!app.world().entity(entity).contains::<FarVoxelReady>());
    }

    #[test]
    fn failed_refresh_keeps_the_previous_ready_representation() {
        let limits = FarVoxelLimits {
            dag: crate::renderer::far_voxel::brick::DagLimits {
                max_reserved_bytes: crate::renderer::far_voxel::brick::ROOT_BYTES
                    + crate::renderer::far_voxel::brick::MIXED_BRICK_BYTES
                    + crate::renderer::far_voxel::residency::GPU_FAR_CELL_RESIDENT_BYTES,
                max_mixed_bricks_per_page: 1,
                max_roots: 1,
            },
            max_reserved_bytes: crate::renderer::far_voxel::brick::ROOT_BYTES
                + crate::renderer::far_voxel::brick::MIXED_BRICK_BYTES
                + crate::renderer::far_voxel::residency::GPU_FAR_CELL_RESIDENT_BYTES
                + crate::renderer::far_voxel::residency::FAR_STATIC_RESIDENT_BYTES,
            max_cells: 1,
            page_bytes: crate::renderer::far_voxel::brick::MIXED_BRICK_BYTES,
        };
        let mut app = brick_app(limits);
        let entity = spawn_cell(&mut app, Vec3::ZERO, 2, BlockId(8));
        assert!(wait_until(&mut app, |app| {
            app.world().entity(entity).contains::<FarVoxelReady>()
        }));
        let before = *app
            .world()
            .entity(entity)
            .get::<FarVoxelResident>()
            .unwrap();
        app.world_mut().get_mut::<LodVoxelData>(entity).unwrap()
            .0 = LodVoxelData::from_dense(&grid(BlockId(9))).0;
        assert!(wait_until(&mut app, |app| {
            !app.world().entity(entity).contains::<FarBrickBuildTask>()
                && app.world().resource::<LodBuildQueue>().pending_count() == 0
        }));

        assert_eq!(
            app.world().entity(entity).get::<FarVoxelResident>(),
            Some(&before)
        );
        assert!(app.world().entity(entity).contains::<FarVoxelReady>());
        assert!(!app.world().entity(entity).contains::<FarRasterFallback>());
    }

    #[test]
    fn same_factor_ready_neighbors_across_distinct_roots_are_written_to_both_cells() {
        let mut app = brick_app(FarVoxelLimits::for_tests());
        let a = spawn_cell(&mut app, Vec3::ZERO, 2, BlockId(1));
        let b = spawn_cell(&mut app, Vec3::new(64.0, 0.0, 0.0), 2, BlockId(2));
        assert!(wait_until(&mut app, |app| {
            let residency = app.world().resource::<FarVoxelResidency>();
            let Some(a_slot) = residency.resident(a).map(|resident| resident.cell_slot) else {
                return false;
            };
            let Some(b_slot) = residency.resident(b).map(|resident| resident.cell_slot) else {
                return false;
            };
            residency
                .cell_metadata(a)
                .and_then(|cell| cell.neighbor(NeighborOffset::new(1, 0, 0).unwrap()))
                == Some(b_slot)
                && residency
                    .cell_metadata(b)
                    .and_then(|cell| cell.neighbor(NeighborOffset::new(-1, 0, 0).unwrap()))
                    == Some(a_slot)
        }));

        let residency = app.world().resource::<FarVoxelResidency>();
        let a_slot = residency.resident(a).unwrap().cell_slot;
        let b_slot = residency.resident(b).unwrap().cell_slot;
        assert_eq!(
            residency
                .cell_metadata(a)
                .unwrap()
                .neighbor(NeighborOffset::new(1, 0, 0).unwrap()),
            Some(b_slot)
        );
        assert_eq!(
            residency
                .cell_metadata(b)
                .unwrap()
                .neighbor(NeighborOffset::new(-1, 0, 0).unwrap()),
            Some(a_slot)
        );
    }

    #[test]
    fn neighbor_refresh_waits_for_nonresident_first_load_backlog_then_batches_scan() {
        let mut app = App::new();
        let first = app.world_mut().spawn_empty().id();
        let second = app.world_mut().spawn_empty().id();
        let backlog = app.world_mut().spawn_empty().id();
        let mut residency = FarVoxelResidency::new(FarVoxelLimits::for_tests());
        let first_prepared = PreparedBrickRoot::from_voxels(&grid(BlockId(21))).unwrap();
        let second_prepared = PreparedBrickRoot::from_voxels(&grid(BlockId(22))).unwrap();
        let FarStageOutcome::Pending {
            batch_id,
            resident: first_resident,
        } = residency.stage_refresh(first, first_prepared, Vec3::new(0.0, -2.0, 0.0), 2)
        else {
            panic!("first cell must stage");
        };
        let FarStageOutcome::Pending {
            batch_id: joined,
            resident: second_resident,
        } = residency.stage_refresh(second, second_prepared, Vec3::new(64.0, -2.0, 0.0), 2)
        else {
            panic!("second cell must stage");
        };
        assert_eq!(joined, batch_id);
        residency.take_ready_upload_batch(2, false).unwrap();
        residency.resolve_upload(FarUploadResult::ack(batch_id));
        app.world_mut().entity_mut(first).insert((
            LodChunk { factor: 2 },
            LodVoxelData::from_dense(&grid(BlockId(21))),
            FarVoxelResident::from_resident(first_resident, 2),
        ));
        app.world_mut().entity_mut(second).insert((
            LodChunk { factor: 2 },
            LodVoxelData::from_dense(&grid(BlockId(22))),
            FarVoxelResident::from_resident(second_resident, 2),
        ));
        let mut queue = LodBuildQueue::default();
        queue.enqueue_brick(backlog);
        app.insert_resource(residency)
            .insert_resource(queue)
            .add_systems(Update, refresh_far_neighbors);

        app.update();
        let residency = app.world().resource::<FarVoxelResidency>();
        assert_eq!(residency.refresh_admission(), FarRefreshAdmission::Any);
        assert_eq!(
            residency
                .cell_metadata(first)
                .unwrap()
                .neighbor(NeighborOffset::new(1, 0, 0).unwrap()),
            None
        );

        app.world_mut()
            .resource_mut::<LodBuildQueue>()
            .remove(backlog);
        app.update();
        let mut residency = app.world_mut().resource_mut::<FarVoxelResidency>();
        assert_eq!(residency.refresh_admission(), FarRefreshAdmission::Blocked);
        let batch = residency.take_upload_batch().unwrap();
        assert_eq!(
            batch
                .uploads
                .iter()
                .filter(|upload| matches!(
                    upload,
                    super::super::residency::FarUpload::WriteCell { .. }
                ))
                .count(),
            2
        );
    }

    #[test]
    fn removed_cell_clear_operations_reach_upload_channel_before_residency_release() {
        let mut app = brick_app(FarVoxelLimits::for_tests());
        let entity = spawn_cell(&mut app, Vec3::ZERO, 2, BlockId(5));
        assert!(wait_until(&mut app, |app| {
            app.world().entity(entity).contains::<FarVoxelReady>()
        }));
        app.world_mut().entity_mut(entity).remove::<LodVoxelData>();
        app.update();

        let channel = app
            .world()
            .resource::<super::super::gpu::FarUploadChannel>()
            .clone();
        let batch = channel.drain_batches().pop().unwrap();
        assert!(matches!(
            batch.uploads[0],
            super::super::residency::FarUpload::ClearCell { .. }
        ));
        assert!(app
            .world()
            .resource::<FarVoxelResidency>()
            .resident(entity)
            .is_some());

        channel.send_result(FarUploadResult::ack(batch.id));
        app.update();
        assert!(app
            .world()
            .resource::<FarVoxelResidency>()
            .resident(entity)
            .is_none());
    }

    #[test]
    fn deferred_removal_wins_the_next_transaction_over_completed_refresh_backlog() {
        let mut app = brick_app(FarVoxelLimits::for_tests());
        let removed = spawn_cell(&mut app, Vec3::ZERO, 2, BlockId(5));
        assert!(wait_until(&mut app, |app| {
            app.world().entity(removed).contains::<FarVoxelReady>()
        }));
        let removed_slot = app
            .world()
            .resource::<FarVoxelResidency>()
            .resident(removed)
            .unwrap()
            .cell_slot;

        app.world_mut().insert_resource(MeshBudgets {
            max_in_flight: 0,
            uploads_per_frame: 8,
        });
        let backlog = spawn_cell(&mut app, Vec3::X * 64.0, 2, BlockId(9));
        app.update();
        let revision = app.world().resource::<FarBrickRevisions>().current(backlog);
        let prepared = PreparedBrickRoot::from_voxels(&grid(BlockId(9))).unwrap();
        let task = AsyncComputeTaskPool::get().spawn(async move { Ok(prepared) });
        app.world_mut()
            .entity_mut(backlog)
            .insert(FarBrickBuildTask { task, revision });
        std::thread::sleep(Duration::from_millis(5));

        app.world_mut().entity_mut(removed).remove::<LodVoxelData>();
        app.update();

        let channel = app
            .world()
            .resource::<super::super::gpu::FarUploadChannel>()
            .clone();
        let batch = channel.drain_batches().pop().unwrap();
        assert!(batch.uploads.iter().any(|upload| matches!(
            upload,
            super::super::residency::FarUpload::ClearCell { slot }
                if *slot == removed_slot
        )));
    }

    #[test]
    fn removal_during_first_upload_never_promotes_deleted_cell_ready() {
        let mut app = brick_app(FarVoxelLimits::for_tests());
        let entity = spawn_cell(&mut app, Vec3::ZERO, 2, BlockId(6));
        for _ in 0..100 {
            app.update();
            if app
                .world()
                .entity(entity)
                .contains::<FarVoxelUploadPending>()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let channel = app
            .world()
            .resource::<super::super::gpu::FarUploadChannel>()
            .clone();
        let first_load = channel.drain_batches().pop().unwrap();
        app.world_mut().entity_mut(entity).remove::<LodVoxelData>();
        app.update();
        channel.send_result(FarUploadResult::ack(first_load.id));
        app.update();

        assert!(!app.world().entity(entity).contains::<FarVoxelReady>());
        let removal = channel.drain_batches().pop().unwrap();
        assert!(matches!(
            removal.uploads[0],
            super::super::residency::FarUpload::ClearCell { .. }
        ));
    }
}
