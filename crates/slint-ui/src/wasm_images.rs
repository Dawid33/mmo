//! wasm32 only: decodes the screens' embedded images in Rust.
//!
//! On wasm32 Slint does not decode embedded images itself: `load_image_from_embedded_data`
//! hands the PNG bytes to the browser as an `HTMLImage`, which the software renderer cannot
//! draw — the first sprite panicked with "not implemented: The image cannot be rendered".
//! build.rs redirects every such call in the generated code here instead, so the browser
//! gets the same decoded pixels as native (where Slint decodes them with the same `image`
//! crate). Slint's own image cache is a 5 MB LRU and would hand an evicted image back to the
//! browser path, so the decoded images are memoised here, keyed like Slint's cache by the
//! embedded data's address; only images some binding actually loads are ever decoded.

use std::cell::RefCell;
use std::collections::HashMap;

use slint::private_unstable_api::re_exports::Slice;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

thread_local! {
    static DECODED: RefCell<HashMap<usize, Image>> = RefCell::new(HashMap::new());
}

/// Same signature as Slint's `load_image_from_embedded_data`, which the generated code calls.
pub(crate) fn load_embedded(data: Slice<'static, u8>, format: Slice<'_, u8>) -> Image {
    let bytes: &'static [u8] = data.as_slice();
    let key = bytes.as_ptr() as usize;
    if let Some(image) = DECODED.with(|d| d.borrow().get(&key).cloned()) {
        return image;
    }
    let format = std::str::from_utf8(format.as_slice()).ok().and_then(image::ImageFormat::from_extension);
    let decoded = match format {
        Some(format) => image::load_from_memory_with_format(bytes, format),
        None => image::load_from_memory(bytes),
    };
    let image = match decoded {
        Ok(decoded) => {
            let rgba = decoded.into_rgba8();
            let (width, height) = rgba.dimensions();
            Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(rgba.as_raw(), width, height))
        }
        // Slint returns an empty image for undecodable data too.
        Err(err) => {
            log::warn!("slint-ui: cannot decode an embedded image: {err}");
            Image::default()
        }
    };
    DECODED.with(|d| d.borrow_mut().insert(key, image.clone()));
    image
}
