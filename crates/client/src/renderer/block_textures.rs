//! Builds the client's block array-texture layers and the
//! `BlockId -> [top, side, bottom]` layer map from the shared `BlockRegistry`.
//! Layer 0 is a procedurally generated magenta/black "missing texture" that is
//! guaranteed present (needs no asset file), so any unresolved texture or
//! unknown block renders as an unmistakable checkerboard rather than silently
//! as another block. Pure/engine-agnostic apart from the Bevy `Resource`
//! derive so it can be unit-tested without a GPU.

use std::collections::BTreeMap;

use bevy::prelude::Resource;
use game::{BlockId, BlockRegistry, TextureSpec};
use image::{Rgba, RgbaImage};

/// Maps each renderable `BlockId` to its `[top, side, bottom]` array-texture
/// layers. The mesher writes these into the `tex_idx` vertex attribute; the
/// fragment shader selects a slot by face normal.
#[derive(Resource, Default, Clone)]
pub struct BlockTextureLayers(pub BTreeMap<BlockId, [u32; 3]>);

/// A high-contrast magenta/black checkerboard, sized to match the block
/// textures so the array texture stays uniform. Reserved as layer 0.
pub fn missing_texture_image(size: u32) -> RgbaImage {
    let cell = (size / 4).max(1);
    let magenta = Rgba([255, 0, 255, 255]);
    let black = Rgba([0, 0, 0, 255]);
    RgbaImage::from_fn(size, size, |x, y| {
        if (x / cell + y / cell) % 2 == 0 { magenta } else { black }
    })
}

/// Intern a texture path to its array layer, loading (once) via `load`.
/// Returns 0 (the missing-texture layer) if the path fails to load.
fn intern<F: FnMut(&str) -> Option<RgbaImage>>(
    path: &str,
    load: &mut F,
    path_layer: &mut BTreeMap<String, u32>,
    images: &mut Vec<RgbaImage>,
    size: &mut Option<u32>,
) -> u32 {
    if let Some(&layer) = path_layer.get(path) {
        return layer;
    }
    match load(path) {
        Some(img) => {
            let (w, h) = img.dimensions(); // inherent on ImageBuffer/RgbaImage
            assert_eq!(w, h, "block texture {path} must be square");
            match *size {
                Some(s) => assert_eq!(s, w, "all block textures must share dimensions"),
                None => *size = Some(w),
            }
            let layer = (images.len() + 1) as u32; // +1: layer 0 is reserved
            images.push(img);
            path_layer.insert(path.to_string(), layer);
            layer
        }
        None => {
            bevy::log::warn!("missing block texture {path:?}; using fallback layer 0");
            0
        }
    }
}

/// Build the ordered array-texture layers (index 0 = checkerboard) and the
/// `BlockId -> [top, side, bottom]` map. Shared paths dedup to one layer;
/// `Untextured` blocks are omitted from the map.
pub fn build_layers<F: FnMut(&str) -> Option<RgbaImage>>(
    registry: &BlockRegistry,
    mut load: F,
) -> (Vec<RgbaImage>, BlockTextureLayers) {
    let mut path_layer: BTreeMap<String, u32> = BTreeMap::new();
    let mut images: Vec<RgbaImage> = Vec::new();
    let mut size: Option<u32> = None;
    let mut map: BTreeMap<BlockId, [u32; 3]> = BTreeMap::new();

    for (id, def) in registry.iter() {
        let triple = match &def.textures {
            TextureSpec::Untextured => continue,
            TextureSpec::All(p) => {
                let l = intern(p, &mut load, &mut path_layer, &mut images, &mut size);
                [l, l, l]
            }
            TextureSpec::Faces { top, side, bottom } => [
                intern(top, &mut load, &mut path_layer, &mut images, &mut size),
                intern(side, &mut load, &mut path_layer, &mut images, &mut size),
                intern(bottom, &mut load, &mut path_layer, &mut images, &mut size),
            ],
        };
        map.insert(id, triple);
    }

    let dim = size.unwrap_or(16);
    let mut layers = Vec::with_capacity(images.len() + 1);
    layers.push(missing_texture_image(dim));
    layers.extend(images);
    (layers, BlockTextureLayers(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::BlockRegistry;
    use image::{Rgba, RgbaImage};

    fn solid(size: u32, px: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(size, size, Rgba(px))
    }

    // Loader that returns a distinct solid image per known path, None otherwise.
    fn loader(path: &str) -> Option<RgbaImage> {
        match path {
            "dirt.png" => Some(solid(16, [100, 60, 20, 255])),
            "grass_top.png" => Some(solid(16, [0, 200, 0, 255])),
            "grass_side.png" => Some(solid(16, [80, 120, 40, 255])),
            _ => None,
        }
    }

    fn registry() -> BlockRegistry {
        BlockRegistry::from_ron(
            r#"(blocks:[
                (id:0,name:"air",textures:Untextured),
                (id:1,name:"dirt",textures:All("dirt.png")),
                (id:2,name:"grass",textures:Faces(top:"grass_top.png",side:"grass_side.png",bottom:"dirt.png")),
            ])"#,
        )
        .unwrap()
    }

    #[test]
    fn layer_zero_is_missing_texture_and_air_is_absent() {
        let reg = registry();
        let (layers, map) = build_layers(&reg, loader);
        assert!(!layers.is_empty(), "must always have the fallback layer");
        assert!(!map.0.contains_key(&BlockId::AIR), "air is never meshed/textured");
    }

    #[test]
    fn all_spec_maps_three_equal_layers() {
        let reg = registry();
        let (_, map) = build_layers(&reg, loader);
        let dirt = map.0[&BlockId(1)];
        assert_eq!(dirt[0], dirt[1]);
        assert_eq!(dirt[1], dirt[2]);
        assert!(dirt[0] >= 1, "real textures live at layers >= 1");
    }

    #[test]
    fn faces_spec_maps_distinct_top_side_bottom_and_dedups_shared() {
        let reg = registry();
        let (_, map) = build_layers(&reg, loader);
        let grass = map.0[&BlockId(2)];
        let dirt = map.0[&BlockId(1)];
        assert_ne!(grass[0], grass[1], "top != side");
        assert_ne!(grass[1], grass[2], "side != bottom");
        assert_eq!(grass[2], dirt[0], "grass bottom shares the dirt.png layer (dedup)");
    }

    #[test]
    fn missing_texture_resolves_to_layer_zero() {
        let reg = BlockRegistry::from_ron(
            r#"(blocks:[(id:0,name:"air",textures:Untextured),(id:1,name:"ghost",textures:All("nope.png"))])"#,
        )
        .unwrap();
        let (_, map) = build_layers(&reg, loader);
        assert_eq!(map.0[&BlockId(1)], [0, 0, 0], "unloadable texture falls back to layer 0");
    }

    #[test]
    fn empty_registry_still_has_fallback_layer() {
        let reg = BlockRegistry::from_ron(r#"(blocks:[(id:0,name:"air",textures:Untextured)])"#).unwrap();
        let (layers, map) = build_layers(&reg, loader);
        assert_eq!(layers.len(), 1, "just the checkerboard");
        assert!(map.0.is_empty());
    }
}
