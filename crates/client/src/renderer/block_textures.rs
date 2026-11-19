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
#[derive(Resource, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockTextureLayers(pub BTreeMap<BlockId, [u32; 3]>);

impl BlockTextureLayers {
    /// Lookup table from a side-face layer to the top-face layer of its block,
    /// identity for every other layer. The pull shader fades distant side faces
    /// — terrace risers too thin to resolve — toward their block's top, so a
    /// grass slope far away reads as grass, not as moiré bands of dirt sides.
    /// A side texture shared by blocks with different tops maps to the last
    /// such block in id order; harmless for a sub-pixel fade.
    pub fn side_to_top(&self) -> Vec<u32> {
        let len = self.0.values().flatten().max().map_or(1, |&max| max as usize + 1);
        let mut lut: Vec<u32> = (0..len as u32).collect();
        for &[top, side, _] in self.0.values() {
            lut[side as usize] = top;
        }
        lut
    }
}

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
            if w != h {
                bevy::log::warn!(
                    "block texture {path:?} is not square ({w}x{h}); using fallback layer 0"
                );
                return 0;
            }
            if let Some(s) = *size {
                if s != w {
                    bevy::log::warn!(
                        "block texture {path:?} size {w} does not match atlas size {s}; using fallback layer 0"
                    );
                    return 0;
                }
            } else {
                *size = Some(w);
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

/// Full mip chain for one block texture layer, level 0 (the image itself)
/// down to 1x1. Distant terrain minifies a block texture far below one texel
/// per pixel; without mips the nearest sampler picks one arbitrary texel per
/// pixel and far ground shimmers as noise, with them it settles on the
/// block's average colour — which is what the real chunk looks like from
/// there. Box-filtered in LINEAR light (the array is sRGB; averaging encoded
/// values darkens) and alpha-weighted, so a transparent texel's hidden colour
/// never bleeds into its neighbours.
pub fn mip_chain(base: &RgbaImage) -> Vec<RgbaImage> {
    let mut levels = vec![base.clone()];
    loop {
        let prev = levels.last().expect("chain starts non-empty");
        let (pw, ph) = prev.dimensions();
        if pw <= 1 && ph <= 1 {
            break;
        }
        let (w, h) = ((pw / 2).max(1), (ph / 2).max(1));
        let next = RgbaImage::from_fn(w, h, |x, y| {
            let (mut rgb, mut alpha, mut n) = ([0.0f32; 3], 0.0f32, 0.0f32);
            for dy in 0..2 {
                for dx in 0..2 {
                    let p = prev.get_pixel((2 * x + dx).min(pw - 1), (2 * y + dy).min(ph - 1));
                    let a = p[3] as f32 / 255.0;
                    for c in 0..3 {
                        rgb[c] += srgb_to_linear(p[c]) * a;
                    }
                    alpha += a;
                    n += 1.0;
                }
            }
            let mut out = [0u8; 4];
            for c in 0..3 {
                out[c] = linear_to_srgb(if alpha > 0.0 { rgb[c] / alpha } else { 0.0 });
            }
            out[3] = (alpha / n * 255.0).round() as u8;
            Rgba(out)
        });
        levels.push(next);
    }
    levels
}

fn srgb_to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let v = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0).round() as u8
}

