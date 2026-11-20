//! Item icons for the Slint screens: an [`ItemId`] → [`slint::Image`] built
//! once from the same block PNGs the item piles draw (`stick` reuses
//! `blocks/oak_branch.png`, see `item_piles::extra_item_texture`). Block
//! items are software-rasterised as a three-face isometric cube; other items
//! remain flat images. The result is made in Rust from decoded RGBA pixels
//! (`SharedPixelBuffer`), which works the same on native and wasm32.
//! An item with no texture of its own gets an empty image, which the slot
//! draws as a tint.

use std::cell::RefCell;
use std::collections::BTreeMap;

use game::{BlockRegistry, ItemId, TextureSpec};
use slint_ui::slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use super::item_piles::extra_item_texture;

const ICON_SIZE: u32 = 64;
const TOP_FACE_MULTIPLIER: u8 = 255;
const LEFT_FACE_MULTIPLIER: u8 = 204;
const RIGHT_FACE_MULTIPLIER: u8 = 153;

thread_local! {
    /// Slint images are `!Send`: one cache per (UI) thread.
    static CACHE: RefCell<IconCache> = RefCell::new(IconCache::default());
}

#[derive(Default)]
struct IconCache {
    registry: Option<BlockRegistry>,
    images: BTreeMap<ItemId, Image>,
}

/// The texture path of `item`, relative to the blocks directory, if it has
/// one (the same mapping the item piles use).
pub fn texture_path(registry: &BlockRegistry, item: ItemId) -> Option<String> {
    match item.as_block() {
        Some(b) => match &registry.def(b)?.textures {
            TextureSpec::Untextured => None,
            TextureSpec::All(p) => Some(p.to_string()),
            TextureSpec::Faces { side, .. } => Some(side.to_string()),
        },
        None => extra_item_texture(item)
            .strip_prefix("blocks/")
            .map(str::to_string),
    }
}

fn block_face_textures(
    registry: &BlockRegistry,
    block: game::BlockId,
) -> Option<[image::RgbaImage; 3]> {
    match &registry.def(block)?.textures {
        TextureSpec::Untextured => None,
        TextureSpec::All(path) => {
            let image = super::load_block_texture(path)?;
            Some([image.clone(), image.clone(), image])
        }
        TextureSpec::Faces { top, side, bottom } => Some([
            super::load_block_texture(top)?,
            super::load_block_texture(side)?,
            super::load_block_texture(bottom)?,
        ]),
    }
}

fn shade(channel: u8, multiplier: u8) -> u8 {
    ((u16::from(channel) * u16::from(multiplier) + 127) / 255) as u8
}

fn blend_pixel(target: &mut image::Rgba<u8>, source: [u8; 4]) {
    let source_alpha = u32::from(source[3]);
    let target_alpha = u32::from(target[3]);
    let inverse_source_alpha = 255 - source_alpha;
    let source_weight = source_alpha * 255;
    let target_weight = target_alpha * inverse_source_alpha;
    let weight = source_weight + target_weight;
    if weight == 0 {
        *target = image::Rgba([0, 0, 0, 0]);
        return;
    }

    for channel in 0..3 {
        target[channel] = ((u32::from(source[channel]) * source_weight
            + u32::from(target[channel]) * target_weight
            + weight / 2)
            / weight) as u8;
    }
    target[3] = ((weight + 127) / 255) as u8;
}

fn raster_face(
    target: &mut image::RgbaImage,
    texture: &image::RgbaImage,
    origin: (f32, f32),
    u_axis: (f32, f32),
    v_axis: (f32, f32),
    multiplier: u8,
) {
    let width = texture.width();
    let height = texture.height();
    if width == 0 || height == 0 {
        return;
    }

    let min_x = origin
        .0
        .min(origin.0 + u_axis.0)
        .min(origin.0 + v_axis.0)
        .min(origin.0 + u_axis.0 + v_axis.0)
        .floor()
        .max(0.0) as u32;
    let max_x = (origin
        .0
        .max(origin.0 + u_axis.0)
        .max(origin.0 + v_axis.0)
        .max(origin.0 + u_axis.0 + v_axis.0)
        .ceil() as u32)
        .min(ICON_SIZE);
    let min_y = origin
        .1
        .min(origin.1 + u_axis.1)
        .min(origin.1 + v_axis.1)
        .min(origin.1 + u_axis.1 + v_axis.1)
        .floor()
        .max(0.0) as u32;
    let max_y = (origin
        .1
        .max(origin.1 + u_axis.1)
        .max(origin.1 + v_axis.1)
        .max(origin.1 + u_axis.1 + v_axis.1)
        .ceil() as u32)
        .min(ICON_SIZE);
    let determinant = u_axis.0 * v_axis.1 - u_axis.1 * v_axis.0;

    for y in min_y..max_y {
        for x in min_x..max_x {
            let relative_x = x as f32 + 0.5 - origin.0;
            let relative_y = y as f32 + 0.5 - origin.1;
            let u = (relative_x * v_axis.1 - relative_y * v_axis.0) / determinant;
            let v = (u_axis.0 * relative_y - u_axis.1 * relative_x) / determinant;
            if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                continue;
            }

            let sample_x = ((u * width as f32).floor() as u32).min(width - 1);
            let sample_y = ((v * height as f32).floor() as u32).min(height - 1);
            let source = texture.get_pixel(sample_x, sample_y);
            if source[3] == 0 {
                continue;
            }
            blend_pixel(
                target.get_pixel_mut(x, y),
                [
                    shade(source[0], multiplier),
                    shade(source[1], multiplier),
                    shade(source[2], multiplier),
                    source[3],
                ],
            );
        }
    }
}

fn block_image(registry: &BlockRegistry, block: game::BlockId) -> Option<image::RgbaImage> {
    let [top, side, _bottom] = block_face_textures(registry, block)?;
    let mut image = image::RgbaImage::new(ICON_SIZE, ICON_SIZE);

    raster_face(
        &mut image,
        &top,
        (32.0, 4.0),
        (28.0, 16.0),
        (-28.0, 16.0),
        TOP_FACE_MULTIPLIER,
    );
    raster_face(
        &mut image,
        &side,
        (4.0, 20.0),
        (28.0, 16.0),
        (0.0, 24.0),
        LEFT_FACE_MULTIPLIER,
    );
    raster_face(
        &mut image,
        &side,
        (32.0, 36.0),
        (28.0, -16.0),
        (0.0, 24.0),
        RIGHT_FACE_MULTIPLIER,
    );
    Some(image)
}

fn flat_image(texture: image::RgbaImage) -> Option<image::RgbaImage> {
    let (width, height) = texture.dimensions();
    if width == 0 || height == 0 {
        return None;
    }

    let mut image = image::RgbaImage::new(ICON_SIZE, ICON_SIZE);
    for y in 0..ICON_SIZE {
        let sample_y = (u64::from(y) * u64::from(height) / u64::from(ICON_SIZE)) as u32;
        for x in 0..ICON_SIZE {
            let sample_x = (u64::from(x) * u64::from(width) / u64::from(ICON_SIZE)) as u32;
            image.put_pixel(x, y, *texture.get_pixel(sample_x, sample_y));
        }
    }
    Some(image)
}

fn render_icon(registry: &BlockRegistry, item: ItemId) -> Option<image::RgbaImage> {
    match item.as_block() {
        Some(block) => block_image(registry, block),
        None => texture_path(registry, item)
            .and_then(|path| super::load_block_texture(&path))
            .and_then(flat_image),
    }
}

/// The final 64×64 icon for `item` (decoded and rasterised once); empty when
/// it has no usable texture.
pub fn icon(item: ItemId) -> Image {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(image) = cache.images.get(&item) {
            return image.clone();
        }
        let registry = cache.registry.get_or_insert_with(|| {
            BlockRegistry::from_ron(&crate::blocks::manifest_src()).expect("invalid block manifest")
        });
        let image = render_icon(registry, item)
            .map(|rgba| {
                Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                    rgba.as_raw(),
                    ICON_SIZE,
                    ICON_SIZE,
                ))
            })
            .unwrap_or_default();
        cache.images.insert(item, image.clone());
        image
    })
}
