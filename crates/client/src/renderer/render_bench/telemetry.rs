use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::render::renderer::RenderAdapterInfo;

use super::output::{AdapterMetadata, UnavailableMetric, UnavailableReason};
use crate::renderer::config::FarRenderBackend;
use crate::renderer::far_voxel::gpu::GpuFarBuffers;
use crate::renderer::far_voxel::pass::FarVoxelPassMetrics;
use crate::renderer::voxel_pull::arena::GpuQuadArena;
use crate::renderer::voxel_pull::gpu::{ActiveNearMode, ForwardCullSource, NearRendererMetrics};

#[derive(Clone, Debug, Default)]
pub struct RenderThreadSnapshot {
    pub adapter: Option<AdapterMetadata>,
    pub near: Option<NearRendererMetrics>,
    pub far_pass: Option<FarVoxelPassMetrics>,
    pub near_logical_bytes: Option<u64>,
    pub near_reserved_bytes: Option<u64>,
    pub far_logical_bytes: Option<u64>,
    pub far_reserved_bytes: Option<u64>,
}

#[derive(Resource, Clone, Default)]
pub struct SharedRenderThreadSnapshot(pub Arc<Mutex<RenderThreadSnapshot>>);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeTelemetry {
    pub active_chunks: Option<u64>,
    pub frustum_survivors: Option<u64>,
    pub hzb_survivors: Option<u64>,
    pub visible_instances: Option<u64>,
    pub resident_quads: Option<u64>,
    pub submitted_quads: Option<u64>,
    pub submitted_triangles: Option<u64>,
    pub submitted_vertices: Option<u64>,
    pub proxy_vertices: Option<u64>,
    pub direct_draws: Option<u64>,
    pub indirect_draws: Option<u64>,
    pub logical_bytes: Option<u64>,
    pub reserved_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranslatedTelemetry {
    pub near_mode: Option<ActiveNearMode>,
    pub near_backend_resolved: Option<String>,
    pub near_visibility_source: Option<ForwardCullSource>,
    pub near: ScopeTelemetry,
    pub far: ScopeTelemetry,
    pub unavailable: Vec<UnavailableMetric>,
}

pub fn capture_render_thread_snapshot(
    adapter: Res<RenderAdapterInfo>,
    arena: Option<Res<GpuQuadArena>>,
    near: Option<Res<NearRendererMetrics>>,
    far_pass: Option<Res<FarVoxelPassMetrics>>,
    far_buffers: Option<Res<GpuFarBuffers>>,
    shared: Res<SharedRenderThreadSnapshot>,
) {
    let mut snapshot = shared
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    snapshot.adapter = Some(AdapterMetadata {
        name: adapter.name.clone(),
        backend: format!("{:?}", adapter.backend),
        device_type: format!("{:?}", adapter.device_type),
        driver: adapter.driver.clone(),
        driver_info: adapter.driver_info.clone(),
        vendor: adapter.vendor,
        device: adapter.device,
    });
    snapshot.near = near.as_deref().copied();
    snapshot.far_pass = far_pass.as_deref().copied();
    snapshot.near_logical_bytes = arena
        .as_deref()
        .map(near_logical_used_bytes)
        .map(saturating_u64);
    snapshot.near_reserved_bytes = arena
        .as_deref()
        .map(near_reserved_bytes)
        .map(saturating_u64);
    snapshot.far_logical_bytes = far_buffers
        .as_deref()
        .map(GpuFarBuffers::logical_used_bytes)
        .map(saturating_u64);
    snapshot.far_reserved_bytes = far_buffers
        .as_deref()
        .map(GpuFarBuffers::reserved_bytes)
        .map(saturating_u64);
}

fn saturating_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn near_logical_used_bytes(arena: &GpuQuadArena) -> usize {
    let record_bytes = arena
        .allocations
        .values()
        .map(|allocation| allocation.quad_count as usize)
        .fold(0usize, usize::saturating_add)
        .saturating_mul(std::mem::size_of::<
            crate::renderer::meshing::records::QuadRecord,
        >());
    let slot_bytes = arena.allocations.len().saturating_mul(
        std::mem::size_of::<Mat4>()
            + std::mem::size_of::<crate::renderer::voxel_pull::arena::GpuNearChunkMeta>(),
    );
    record_bytes.saturating_add(slot_bytes)
}

fn near_reserved_bytes(arena: &GpuQuadArena) -> usize {
    arena
        .records
        .capacity()
        .saturating_mul(std::mem::size_of::<
            crate::renderer::meshing::records::QuadRecord,
        >())
        .saturating_add(
            arena
                .transforms
                .capacity()
                .saturating_mul(std::mem::size_of::<Mat4>()),
        )
        .saturating_add(
            arena
                .metadata
                .capacity()
                .saturating_mul(std::mem::size_of::<
                    crate::renderer::voxel_pull::arena::GpuNearChunkMeta,
                >()),
        )
}

pub fn translate_snapshot(
    snapshot: &RenderThreadSnapshot,
    far_backend: FarRenderBackend,
) -> TranslatedTelemetry {
    let mut translated = TranslatedTelemetry::default();
    if let Some(near) = snapshot.near {
        translated.near_mode = Some(near.active_mode);
        translated.near_backend_resolved = Some(near.active_mode.as_str().to_owned());
        translated.near_visibility_source = near.visibility_source;
        translated.near.active_chunks = Some(u64::from(near.active_chunks));
        translated.near.frustum_survivors = near.frustum_visible_chunks.map(u64::from);
        translated.near.hzb_survivors = near.hzb_visible_chunks.map(u64::from);
        translated.near.resident_quads = Some(near.resident_quads);
        translated.near.submitted_quads = near.submitted_quads;
        translated.near.submitted_triangles =
            near.submitted_quads.map(|quads| quads.saturating_mul(2));
        translated.near.submitted_vertices = near.submitted_vertices;
        translated.near.indirect_draws = near.indirect_draw_count.map(u64::from);
        translated.near.direct_draws = near.direct_draw_count.map(u64::from);
    }
    translated.near.logical_bytes = snapshot.near_logical_bytes;
    translated.near.reserved_bytes = snapshot.near_reserved_bytes;
    translated.far.logical_bytes = snapshot.far_logical_bytes;
    translated.far.reserved_bytes = snapshot.far_reserved_bytes;
    if let Some(far) = snapshot.far_pass {
        translated.far.indirect_draws = Some(u64::from(far.indirect_draws));
        translated.far.active_chunks = Some(u64::from(far.active_cells));
        translated.far.visible_instances = far.visible_instances.map(u64::from);
        translated.far.proxy_vertices = far.visible_instances.map(|instances| {
            u64::from(instances).saturating_mul(u64::from(
                crate::renderer::far_voxel::pipeline::PROXY_VERTEX_COUNT,
            ))
        });
    }
    if far_backend == FarRenderBackend::BrickRaymarch {
        translated.unavailable.push(UnavailableMetric {
            path: "metrics.geometry.far.resident_quads".to_owned(),
            reason: UnavailableReason::NotApplicableToBackend,
            detail: Some("brick raymarching does not store raster quads".to_owned()),
        });
        if translated.far.visible_instances.is_none() {
            translated.unavailable.push(UnavailableMetric {
                path: "metrics.geometry.far.proxy_vertices".to_owned(),
                reason: UnavailableReason::SampleNotObserved,
                detail: Some(
                    "asynchronous visible-instance readback produced no sample".to_owned(),
                ),
            });
        }
    }
    translated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_exact_near_geometry_without_scope_ambiguity() {
        let snapshot = RenderThreadSnapshot {
            near: Some(NearRendererMetrics {
                active_mode: ActiveNearMode::GpuDriven {
                    indirect_count: true,
                },
                mode_generation: 1,
                visibility_source: Some(ForwardCullSource::Hzb),
                active_chunks: 17,
                frustum_visible_chunks: Some(11),
                hzb_visible_chunks: Some(7),
                indirect_draw_count: Some(3),
                direct_draw_count: Some(0),
                resident_quads: 41,
                submitted_quads: Some(23),
                submitted_vertices: Some(138),
            }),
            far_pass: Some(FarVoxelPassMetrics {
                indirect_draws: 1,
                ..Default::default()
            }),
            near_logical_bytes: Some(100),
            near_reserved_bytes: Some(200),
            far_logical_bytes: Some(300),
            far_reserved_bytes: Some(400),
            ..Default::default()
        };

        let translated = translate_snapshot(&snapshot, FarRenderBackend::Raster);
        assert_eq!(translated.near.active_chunks, Some(17));
        assert_eq!(translated.near.frustum_survivors, Some(11));
        assert_eq!(translated.near.hzb_survivors, Some(7));
        assert_eq!(translated.near.resident_quads, Some(41));
        assert_eq!(translated.near.submitted_quads, Some(23));
        assert_eq!(translated.near.submitted_triangles, Some(46));
        assert_eq!(translated.near.submitted_vertices, Some(138));
        assert_eq!(translated.near.direct_draws, Some(0));
        assert_eq!(translated.far.indirect_draws, Some(1));
        assert_eq!(translated.near.logical_bytes, Some(100));
        assert_eq!(translated.far.reserved_bytes, Some(400));
    }

    #[test]
    fn translation_carries_the_actual_render_world_backend_and_visibility_source() {
        let snapshot = RenderThreadSnapshot {
            near: Some(NearRendererMetrics {
                active_mode: crate::renderer::voxel_pull::gpu::ActiveNearMode::GroupedDirect,
                visibility_source: Some(
                    crate::renderer::voxel_pull::gpu::ForwardCullSource::Frustum,
                ),
                ..Default::default()
            }),
            ..Default::default()
        };

        let translated = translate_snapshot(&snapshot, FarRenderBackend::Raster);

        assert_eq!(translated.near_backend_resolved.as_deref(), Some("direct"));
        assert_eq!(
            translated.near_visibility_source,
            Some(crate::renderer::voxel_pull::gpu::ForwardCullSource::Frustum)
        );
    }

    #[test]
    fn brick_geometry_distinguishes_proxy_vertices_from_raster_quads() {
        let snapshot = RenderThreadSnapshot {
            far_pass: Some(FarVoxelPassMetrics {
                visible_instances: Some(5),
                ..Default::default()
            }),
            ..Default::default()
        };
        let translated = translate_snapshot(&snapshot, FarRenderBackend::BrickRaymarch);
        assert_eq!(translated.far.resident_quads, None);
        assert_eq!(translated.far.proxy_vertices, Some(36 * 5));
        assert!(translated.unavailable.iter().any(|metric| {
            metric.path == "metrics.geometry.far.resident_quads"
                && metric.reason == UnavailableReason::NotApplicableToBackend
        }));
    }

    #[test]
    fn near_logical_memory_counts_live_allocations_not_sparse_backing_length() {
        let mut arena = GpuQuadArena::default();
        arena.ensure_allocation(Entity::from_bits(1), 9);
        arena.ensure_allocation(Entity::from_bits(2), 3);
        arena.release(Entity::from_bits(1));

        assert_eq!(
            near_logical_used_bytes(&arena),
            3 * std::mem::size_of::<crate::renderer::meshing::records::QuadRecord>()
                + std::mem::size_of::<Mat4>()
                + std::mem::size_of::<crate::renderer::voxel_pull::arena::GpuNearChunkMeta>()
        );
    }
}
