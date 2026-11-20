//! `BlockId → RGB` for the map: the last level of each block's TOP layer's mip chain
//! (`block_textures::mip_chain` is alpha-weighted, so a cutout block's colour is its opaque texels'
//! mean), uploaded as a one-row texture indexed by block id.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use game::BlockRegistry;
use image::RgbaImage;

use crate::renderer::block_textures::{mip_chain, BlockTextureLayers};

/// A block with no texture (or none resolved).
pub const MAP_FALLBACK_RGB: [u8; 4] = [128, 128, 128, 255];

#[derive(Resource, Clone)]
pub struct MapPalette {
    /// Index = `BlockId.0`.
    pub rgb: Vec<[u8; 4]>,
    pub texture: Handle<Image>,
}

/// Every block's map colour, index = block id (`layers` as `build_layers` made them).
pub fn block_colours(registry: &BlockRegistry, layers: &[RgbaImage], block_layers: &BlockTextureLayers) -> Vec<[u8; 4]> {
    let len = registry.iter().map(|(id, _)| id.0 as usize + 1).max().unwrap_or(1);
    let mut rgb = vec![MAP_FALLBACK_RGB; len];
    for (id, [top, _, _]) in &block_layers.0 {
        let Some(layer) = layers.get(*top as usize) else { continue };
        let chain = mip_chain(layer);
        let p = chain.last().expect("a chain has a level").get_pixel(0, 0);
        if (id.0 as usize) < len {
            rgb[id.0 as usize] = [p[0], p[1], p[2], 255];
        }
    }
    rgb
}

/// The palette resource, its texture added to `images`.
pub fn make_palette(images: &mut Assets<Image>, rgb: Vec<[u8; 4]>) -> MapPalette {
    let mut image = Image::new(
        Extent3d { width: rgb.len() as u32, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        rgb.iter().flatten().copied().collect(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::nearest());
    MapPalette { rgb, texture: images.add(image) }
}
