//! PNG output for the CLI: the colour code lives in `worldgen::forge::render`.

use image::{ImageBuffer, Luma};

use worldgen::forge::render::{encode_png, render, PreviewLayer};
use worldgen::forge::{Forged, World};

// Engine frame, for the 16-bit heightmap (same mapping as examples/world_map.rs).
const SEA_LEVEL_Y: f64 = 64.0;
const HEIGHT_MIN_Y: f64 = -64.0;
const HEIGHT_MAX_Y: f64 = 320.0;

pub fn png_bytes(forged: &Forged, layer: PreviewLayer) -> Vec<u8> {
    encode_png(&render(forged, layer))
}

pub fn layer(forged: &Forged, layer: PreviewLayer, path: &str) {
    std::fs::write(path, png_bytes(forged, layer)).expect("write png");
    println!("wrote {path}");
}

pub fn heightmap(w: &World, path: &str) {
    let mut gray = ImageBuffer::<Luma<u16>, Vec<u16>>::new(w.n as u32, w.n as u32);
    for i in 0..w.n * w.n {
        let t = (SEA_LEVEL_Y + w.h[i] - HEIGHT_MIN_Y) / (HEIGHT_MAX_Y - HEIGHT_MIN_Y);
        let v = (t.clamp(0.0, 1.0) * 65535.0) as u16;
        gray.put_pixel((i % w.n) as u32, (i / w.n) as u32, Luma([v]));
    }
    gray.save(path).expect("write heightmap");
    println!("wrote {path}");
}
