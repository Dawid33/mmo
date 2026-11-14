//! The picture: plan, south elevation and east elevation side by side.

use std::collections::BTreeMap;
use std::io::BufWriter;
use std::path::Path;

use game::prefab::{Cell, ResolvedPrefab, ResolvedShape, PREFAB_BUCKET_BLOCKS};
use game::registry::{BlockId, BlockRegistry, TextureSpec};

pub const GAP: u32 = 4;
const MAX_WIDTH: u32 = 2048;
const BACKGROUND: [u8; 3] = [24, 24, 28];
const GREY: [u8; 3] = [128, 128, 128];
const MARKER: [u8; 3] = [255, 0, 255];
/// Plan shading: the lowest top keeps this share of its colour, the highest all of it.
const LOW_SHADE: f32 = 0.6;

pub struct Image {
    pub width: u32,
    pub height: u32,
    /// RGB, row-major.
    pub rgb: Vec<u8>,
}

impl Image {
    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, c: [u8; 3]) {
        for yy in y..(y + h).min(self.height) {
            for xx in x..(x + w).min(self.width) {
                let i = ((yy * self.width + xx) * 3) as usize;
                self.rgb[i..i + 3].copy_from_slice(&c);
            }
        }
    }
}

/// Mean colour of each block's side texture.
pub struct BlockColours {
    by_id: BTreeMap<BlockId, [u8; 3]>,
}

impl BlockColours {
    pub fn load(registry: &BlockRegistry, blocks_dir: &Path) -> Self {
        let mut by_id = BTreeMap::new();
        for (id, def) in registry.iter() {
            let side = match &def.textures {
                TextureSpec::Untextured => None,
                TextureSpec::All(p) => Some(p),
                TextureSpec::Faces { side, .. } => Some(side),
            };
            if let Some(c) = side.and_then(|p| mean_colour(&blocks_dir.join(p))) {
                by_id.insert(id, c);
            }
        }
        Self { by_id }
    }

    fn of(&self, id: BlockId) -> [u8; 3] {
        self.by_id.get(&id).copied().unwrap_or(GREY)
    }
}

/// Mean of the opaque texels (alpha >= 128); `None` for an unreadable or fully transparent texture.
fn mean_colour(path: &Path) -> Option<[u8; 3]> {
    let mut decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return None,
    };
    let (mut sum, mut n) = ([0u64; 3], 0u64);
    for px in buf[..info.buffer_size()].chunks_exact(channels) {
        let (rgb, alpha) = match channels {
            1 => ([px[0]; 3], 255),
            2 => ([px[0]; 3], px[1]),
            3 => ([px[0], px[1], px[2]], 255),
            _ => ([px[0], px[1], px[2]], px[3]),
        };
        if alpha >= 128 {
            for k in 0..3 {
                sum[k] += rgb[k] as u64;
            }
            n += 1;
        }
    }
    (n > 0).then(|| sum.map(|s| (s / n) as u8))
}

/// The largest whole scale keeping the picture within 2048 px wide; at least 1.
pub fn default_scale(size: [u32; 3]) -> u32 {
    let blocks_wide = 2 * size[0] + size[2];
    (MAX_WIDTH.saturating_sub(2 * GAP) / blocks_wide.max(1)).max(1)
}

fn cell_material(cell: Cell<'_>) -> Option<BlockId> {
    match cell {
        Cell::Block(id) => Some(id),
        Cell::Shaped(s) => ResolvedShape::top_material(s),
        Cell::Keep | Cell::Carve => None,
    }
}

/// Panels, left to right: plan (north up), south elevation (looking north, east to the right), east
/// elevation (looking west, south to the left). Elevations stand on the picture's bottom edge.
pub fn render(p: &ResolvedPrefab, colours: &BlockColours, scale: u32) -> Image {
    let [sx, sy, sz] = p.size;
    let width = (2 * sx + sz) * scale + 2 * GAP;
    let height = sz.max(sy) * scale;
    let mut img = Image { width, height, rgb: BACKGROUND.repeat((width * height) as usize) };

    draw_plan(&mut img, p, colours, scale);
    let south = elevation(p, colours, Side::South);
    blit(&mut img, &south, sx, sy, sx * scale + GAP, scale);
    let east = elevation(p, colours, Side::East);
    blit(&mut img, &east, sz, sy, 2 * sx * scale + 2 * GAP, scale);
    img
}

fn draw_plan(img: &mut Image, p: &ResolvedPrefab, colours: &BlockColours, scale: u32) {
    let [sx, _, sz] = p.size.map(|v| v as i32);
    let tops = (0..sz).flat_map(|z| (0..sx).map(move |x| (x, z)));
    let (mut lo, mut hi) = (i32::MAX, i32::MIN);
    for (x, z) in tops.clone() {
        if let Some((y, _)) = p.column_top(x, z) {
            lo = lo.min(y);
            hi = hi.max(y);
        }
    }
    for (x, z) in tops {
        let Some((y, id)) = p.column_top(x, z) else { continue };
        let shade = if hi > lo { LOW_SHADE + (1.0 - LOW_SHADE) * (y - lo) as f32 / (hi - lo) as f32 } else { 1.0 };
        let c = colours.of(id).map(|v| (v as f32 * shade).round() as u8);
        img.fill(x as u32 * scale, z as u32 * scale, scale, scale, c);
    }
    for m in &p.markers {
        if (0..sx).contains(&m.at[0]) && (0..sz).contains(&m.at[2]) {
            // One pixel at the centre of the block's square.
            img.fill(m.at[0] as u32 * scale + scale / 2, m.at[2] as u32 * scale + scale / 2, 1, 1, MARKER);
        }
    }
}

#[derive(Clone, Copy)]
enum Side {
    South,
    East,
}

/// `width x sy` colours, row 0 the top (highest y). South: column = x, looking along -z, so the largest
/// z wins. East: column = sz-1-z (south at the left), looking along -x, so the largest x wins.
fn elevation(p: &ResolvedPrefab, colours: &BlockColours, side: Side) -> Vec<Option<[u8; 3]>> {
    let [sx, sy, sz] = p.size.map(|v| v as i32);
    let (w, depth_len) = match side {
        Side::South => (sx, sz),
        Side::East => (sz, sx),
    };
    let mut pixels: Vec<Option<[u8; 3]>> = vec![None; (w * sy) as usize];
    let mut open = (w * sy) as usize;
    // Slabs of one bucket's thickness along the viewing axis, nearest first: memory is one slab's cells.
    let mut hi = depth_len;
    while hi > 0 && open > 0 {
        let lo = (hi - PREFAB_BUCKET_BLOCKS).max(0);
        let (min, max) = match side {
            Side::South => ([0, 0, lo], [sx, sy, hi]),
            Side::East => ([lo, 0, 0], [hi, sy, sz]),
        };
        let mut found: Vec<(usize, i32, [u8; 3])> = Vec::new();
        p.for_each_cell(min, max, &mut |at, cell| {
            let Some(id) = cell_material(cell) else { return };
            let (col, d) = match side {
                Side::South => (at[0], at[2]),
                Side::East => (sz - 1 - at[2], at[0]),
            };
            found.push(((at[1] * w + col) as usize, d, colours.of(id)));
        });
        // Rows are only filled by the nearest slab that has anything at that pixel.
        let mut slab_depth: BTreeMap<usize, (i32, [u8; 3])> = BTreeMap::new();
        for (i, d, c) in found {
            if pixels[i].is_none() && slab_depth.get(&i).is_none_or(|(best, _)| d > *best) {
                slab_depth.insert(i, (d, c));
            }
        }
        for (i, (_, c)) in slab_depth {
            pixels[i] = Some(c);
            open -= 1;
        }
        hi = lo;
    }
    // Flip rows: index 0 of `pixels` is y = 0, the picture wants the top first.
    let mut flipped = Vec::with_capacity(pixels.len());
    for y in (0..sy).rev() {
        flipped.extend_from_slice(&pixels[(y * w) as usize..((y + 1) * w) as usize]);
    }
    flipped
}

fn blit(img: &mut Image, panel: &[Option<[u8; 3]>], w: u32, h: u32, x0: u32, scale: u32) {
    let y0 = img.height - h * scale;
    for row in 0..h {
        for col in 0..w {
            if let Some(c) = panel[(row * w + col) as usize] {
                img.fill(x0 + col * scale, y0 + row * scale, scale, scale, c);
            }
        }
    }
}

pub fn write_png(path: &Path, img: &Image) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(BufWriter::new(file), img.width, img.height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(&img.rgb).map_err(|e| e.to_string())
}
