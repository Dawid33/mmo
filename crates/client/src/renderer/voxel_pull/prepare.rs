//! Render-world buffer/bind-group upload for the pulled render path. Runs in
//! `PrepareBindGroups`; only changed chunks re-upload (steady state is a no-op).

use bevy::{
    prelude::*,
    render::{
        render_asset::RenderAssets,
        render_resource::*,
        renderer::{RenderDevice, RenderQueue},
        texture::GpuImage,
    },
};

use super::pipeline::{ChunkUniform, VoxelPullPipeline};
use super::{BlockArrayTexture, ExtractedChunkQuads};

/// Per-chunk GPU state: the group(1) bind group and the quad count that drives
/// the `quad_count * 6` vertex draw. Tasks 6/7 read this back for depth passes.
#[derive(Component)]
pub struct GpuChunkQuads {
    pub bind_group: BindGroup,
    pub quad_count: u32,
}

pub fn prepare_chunk_quads(
    mut commands: Commands,
    chunks: Query<(Entity, &ExtractedChunkQuads)>,
    existing: Query<&GpuChunkQuads>,
    pipeline: Res<VoxelPullPipeline>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    images: Res<RenderAssets<GpuImage>>,
    block_texture: Res<BlockArrayTexture>,
) {
    let Some(texture) = images.get(&block_texture.0) else {
        return; // array texture not uploaded yet — retry next frame
    };
    for (entity, extracted) in &chunks {
        if !extracted.changed && existing.contains(entity) {
            continue; // steady state: nothing to upload
        }
        let quad_count = extracted.records.len() as u32;
        let records = device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("voxel_pull_records"),
            contents: bytemuck::cast_slice(&extracted.records),
            usage: BufferUsages::STORAGE,
        });
        let uniform = ChunkUniform {
            world_from_local: extracted.world_from_local,
            quad_count,
        };
        let mut buf = UniformBuffer::from(uniform);
        buf.write_buffer(&device, &queue);
        let bind_group = device.create_bind_group(
            "voxel_pull_chunk",
            &pipeline.chunk_layout,
            &BindGroupEntries::sequential((
                records.as_entire_binding(),
                buf.binding().unwrap(),
                &texture.texture_view,
                &texture.sampler,
            )),
        );
        commands
            .entity(entity)
            .insert(GpuChunkQuads { bind_group, quad_count });
    }
}
