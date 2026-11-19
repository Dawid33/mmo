use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::render::renderer::RenderAdapterInfo;

use super::output::AdapterMetadata;
use crate::renderer::voxel_pull::arena::GpuQuadArena;
use crate::renderer::voxel_pull::gpu::{ActiveNearMode, ForwardCullSource, NearRendererMetrics};

#[derive(Clone, Debug, Default)]
pub struct RenderThreadSnapshot {
    pub adapter: Option<AdapterMetadata>,
    pub near: Option<NearRendererMetrics>,
    pub near_logical_bytes: Option<u64>,
    pub near_reserved_bytes: Option<u64>,
}

#[derive(Resource, Clone, Default)]
pub struct SharedRenderThreadSnapshot(pub Arc<Mutex<RenderThreadSnapshot>>);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeTelemetry {
    pub active_chunks: Option<u64>,
    pub frustum_survivors: Option<u64>,
    pub hzb_survivors: Option<u64>,
    pub resident_quads: Option<u64>,
    pub submitted_quads: Option<u64>,
    pub submitted_triangles: Option<u64>,
    pub submitted_vertices: Option<u64>,
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
}

pub fn capture_render_thread_snapshot(
    adapter: Res<RenderAdapterInfo>,
    arena: Option<Res<GpuQuadArena>>,
    near: Option<Res<NearRendererMetrics>>,
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
    snapshot.near_logical_bytes = arena
        .as_deref()
        .map(near_logical_used_bytes)
        .map(saturating_u64);
    snapshot.near_reserved_bytes = arena
        .as_deref()
        .map(near_reserved_bytes)
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

pub fn translate_snapshot(snapshot: &RenderThreadSnapshot) -> TranslatedTelemetry {
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
    translated
}

