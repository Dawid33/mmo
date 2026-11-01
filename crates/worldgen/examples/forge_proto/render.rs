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
    let mut gray = ImageBuffer::<Luma<u16>, Vec<u16>>::new(w.n as u32, w.n as u32);
    for i in 0..w.n * w.n {
        let t = (SEA_LEVEL_Y + w.h[i] - HEIGHT_MIN_Y) / (HEIGHT_MAX_Y - HEIGHT_MIN_Y);
        let v = (t.clamp(0.0, 1.0) * 65535.0) as u16;
        gray.put_pixel((i % w.n) as u32, (i / w.n) as u32, Luma([v]));
    }
    gray.save(path).expect("write heightmap");
    println!("wrote {path}");
}

/// Debug biome map: one pixel per cell, a flat colour per `game::Biome`
/// (the world map's biome layer comes later; this is for the CLI only).
pub fn biomes(a: &worldgen::forged::WorldArtifact, path: &str) {
    use game::Biome::*;
    let colour = |b: game::Biome| -> [u8; 3] {
        match b {
            Ocean => [40, 80, 160],
            FrozenOcean => [170, 200, 225],
            Lake => [70, 150, 215],
            Glacier => [245, 250, 255],
            Tundra => [175, 185, 165],
            Taiga => [60, 105, 85],
            ConiferForest => [35, 110, 60],
            BroadleafForest => [70, 150, 50],
            TropicalConiferForest => [20, 120, 90],
            TropicalBroadleafForest => [10, 90, 30],
            Grassland => [165, 200, 90],
            Savanna => [200, 190, 90],
            Shrubland => [150, 160, 95],
            SandDesert => [235, 215, 150],
            RockyWasteland => [170, 140, 110],
            Badlands => [190, 100, 60],
            Marsh => [100, 160, 140],
            Swamp => [60, 110, 90],
            MangroveSwamp => [130, 60, 140],
            Mountain => [120, 115, 110],
        }
    };
    let n = a.n;
    let mut img = image::RgbImage::new(n, n);
    for (i, &b) in a.biome.iter().enumerate() {
        let c = colour(game::Biome::from_u8(b).unwrap_or(Ocean));
        img.put_pixel(i as u32 % n, i as u32 / n, image::Rgb(c));
    }
    img.save(path).expect("write biome png");
    println!("wrote {path}");
}
