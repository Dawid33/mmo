//! Item icons for the Slint screens: an [`ItemId`] → [`slint::Image`] built
//! once from the same block PNGs the item piles draw (`stick` reuses
//! `blocks/oak_branch.png`, see `item_piles::extra_item_texture`). The image
//! is made in Rust from the decoded RGBA pixels (`SharedPixelBuffer`), which
//! works the same on native and wasm32 — no embedded-image loader involved.
//! An item with no texture of its own gets an empty image, which the slot
//! draws as a tint.

use std::cell::RefCell;
use std::collections::BTreeMap;

use game::{BlockRegistry, ItemId, TextureSpec};
use slint_ui::slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use super::item_piles::extra_item_texture;

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
        None => extra_item_texture(item).strip_prefix("blocks/").map(str::to_string),
    }
}

/// The icon for `item` (decoded once); empty when it has no texture.
pub fn icon(item: ItemId) -> Image {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(image) = cache.images.get(&item) {
            return image.clone();
        }
        let registry = cache.registry.get_or_insert_with(|| {
            BlockRegistry::from_ron(&crate::blocks::manifest_src()).expect("invalid block manifest")
        });
        let image = texture_path(registry, item)
            .and_then(|path| super::load_block_texture(&path))
            .map(|rgba| {
                let (w, h) = rgba.dimensions();
                Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(rgba.as_raw(), w, h))
            })
            .unwrap_or_default();
        cache.images.insert(item, image.clone());
        image
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stick_reuses_the_oak_branch_texture() {
        let registry = BlockRegistry::from_ron(&crate::blocks::manifest_src()).unwrap();
        let stick = ItemId::extra_named("stick").expect("stick item");
        assert_eq!(texture_path(&registry, stick).as_deref(), Some("oak_branch.png"));
    }
}
