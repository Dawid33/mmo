//! PNG output for the CLI: the colour code lives in `worldgen::forge::render`.

use image::{ImageBuffer, Luma};

use worldgen::forge::render::{encode_png, render, PreviewLayer};
use worldgen::forge::{Forged, World};

// Engine frame, for the 16-bit heightmap (same mapping as examples/world_map.rs:
// -1,000..+3,600 m, which holds the tallest `max_peak_m` above the sea).
const SEA_LEVEL_Y: f64 = 64.0;
const HEIGHT_MIN_Y: f64 = -1_000.0;
const HEIGHT_MAX_Y: f64 = 3_600.0;

pub fn png_bytes(forged: &Forged, layer: PreviewLayer) -> Vec<u8> {
    encode_png(&render(forged, layer))
}

pub fn layer(forged: &Forged, layer: PreviewLayer, path: &str) {
    std::fs::write(path, png_bytes(forged, layer)).expect("write png");
    println!("wrote {path}");
}

pub fn heightmap(w: &World, path: &str) {
    let mut gray = ImageBuffer::<Luma<u16>, Vec<u16>>::new(w.nx as u32, w.nz as u32);
    for i in 0..w.cells() {
        let t = (SEA_LEVEL_Y + w.h[i] - HEIGHT_MIN_Y) / (HEIGHT_MAX_Y - HEIGHT_MIN_Y);
        let v = (t.clamp(0.0, 1.0) * 65535.0) as u16;
        let (x, z) = w.xz(i);
        gray.put_pixel(x as u32, z as u32, Luma([v]));
    }
    gray.save(path).expect("write heightmap");
    println!("wrote {path}");
}

/// Debug biome map: one pixel per cell, a flat colour per `game::Biome`
/// from the shared palette (`Biome::map_rgb`, also the world map's biome layer).
pub fn biomes(a: &worldgen::forged::WorldArtifact, path: &str) {
    let colour = |b: game::Biome| b.map_rgb();
    let (nx, nz) = (a.nx, a.nz);
    let mut img = image::RgbImage::new(nx, nz);
    for (i, &b) in a.biome.iter().enumerate() {
        let c = colour(game::Biome::from_u8(b).unwrap_or(game::Biome::Ocean));
        img.put_pixel(i as u32 % nx, i as u32 / nx, image::Rgb(c));
    }
    img.save(path).expect("write biome png");
    println!("wrote {path}");
}
