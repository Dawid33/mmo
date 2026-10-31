//! Preview rendering of a `Forged` world: colour relief, rivers, geology,
//! rainfall and the pre-erosion relief, as RGBA8 images (the generator
//! screen's live preview and a world's thumbnail).

use image::{ImageBuffer, Rgb};

use super::geology::ROCKS;
use super::hydrology::{Analysis, LAKE_MIN_DEPTH_M};
use super::{Forged, World};

const RIVER_RGB: [f64; 3] = [40.0, 105.0, 200.0];
const LAKE_RGB: [u8; 3] = [70, 150, 215];
/// River line radius in pixels by Strahler order (drawn wider than true scale:
/// a 20 m river is ~1 px at 16 m cells).
const RIVER_RADIUS_PX: (f64, f64) = (0.25, 0.5); // base + per order

/// Which view of the forged world to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewLayer {
    Relief,
    Rivers,
    Geology,
    Rain,
    BeforeErosion,
}

impl PreviewLayer {
    pub const ALL: [PreviewLayer; 5] =
        [Self::Relief, Self::Rivers, Self::Geology, Self::Rain, Self::BeforeErosion];

    pub fn label(self) -> &'static str {
        match self {
            Self::Relief => "Relief",
            Self::Rivers => "Rivers",
            Self::Geology => "Geology",
            Self::Rain => "Rain",
            Self::BeforeErosion => "Before erosion",
        }
    }
}

/// RGBA8, row-major, top row = north (min z).
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The numbers shown beside a preview.
#[derive(Clone, Debug)]
pub struct PreviewStats {
    pub land_share: f64,
    pub islands: usize,
    pub largest_island_km2: f64,
    pub highest_peak_m: f64,
    pub lakes: usize,
    pub river_reaches: usize,
}

pub fn stats(forged: &Forged) -> PreviewStats {
    let st = &forged.analysis.stats;
    PreviewStats {
        land_share: st.land_share,
        islands: st.islands,
        largest_island_km2: st.largest_island_km2,
        highest_peak_m: st.heights[4],
        lakes: st.lake_count,
        river_reaches: forged.analysis.rivers.len(),
    }
}

pub fn render(forged: &Forged, layer: PreviewLayer) -> Rgba {
    let (w, a) = (&forged.world, &forged.analysis);
    let img = match layer {
        PreviewLayer::Relief => relief_image(w, None),
        PreviewLayer::Rivers => relief_image(w, Some(a)),
        PreviewLayer::Geology => geology_image(w, a),
        PreviewLayer::Rain => rainfall_image(w, a, &forged.rain),
        PreviewLayer::BeforeErosion => relief_image(&forged.initial, None),
    };
    let (width, height) = img.dimensions();
    let mut pixels = Vec::with_capacity(img.len() / 3 * 4);
    for p in img.pixels() {
        pixels.extend_from_slice(&[p.0[0], p.0[1], p.0[2], 255]);
    }
    Rgba { width, height, pixels }
}

pub fn encode_png(img: &Rgba) -> Vec<u8> {
    let buf = ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(img.width, img.height, &img.pixels[..])
        .expect("pixel buffer matches dimensions");
    let mut bytes = std::io::Cursor::new(Vec::new());
    buf.write_to(&mut bytes, image::ImageFormat::Png).expect("encode png");
    bytes.into_inner()
}

type Image = ImageBuffer<Rgb<u8>, Vec<u8>>;

fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn ramp(stops: &[(f64, [f64; 3])], v: f64) -> [f64; 3] {
    let mut prev = stops[0];
    for &stop in stops {
        if v <= stop.0 {
            return lerp3(prev.1, stop.1, (v - prev.0) / (stop.0 - prev.0).max(f64::EPSILON));
        }
        prev = stop;
    }
    prev.1
}

fn sea_color(depth: f64) -> [f64; 3] {
    ramp(
        &[
            (0.0, [110.0, 190.0, 215.0]),
            (8.0, [60.0, 140.0, 200.0]),
            (40.0, [30.0, 80.0, 160.0]),
            (128.0, [12.0, 30.0, 90.0]),
        ],
        depth,
    )
}

/// Hillshade factor for a land cell: lit from the north-west, relief x2.
fn shade(w: &World, x: usize, z: usize) -> f64 {
    let n = w.n;
    let sun = [-0.5f64, std::f64::consts::FRAC_1_SQRT_2, -0.5];
    let at = |x: usize, z: usize| w.h[z * n + x].max(0.0);
    let (xl, xr) = (x.saturating_sub(1), (x + 1).min(n - 1));
    let (zu, zd) = (z.saturating_sub(1), (z + 1).min(n - 1));
    let dhdx = (at(xr, z) - at(xl, z)) / ((xr - xl) as f64 * w.dx);
    let dhdz = (at(x, zd) - at(x, zu)) / ((zd - zu) as f64 * w.dx);
    let nrm = [-dhdx * 2.0, 1.0, -dhdz * 2.0];
    let len = (nrm[0] * nrm[0] + 1.0 + nrm[2] * nrm[2]).sqrt();
    let lambert = (nrm[0] * sun[0] + sun[1] + nrm[2] * sun[2]) / len;
    (0.35 + 0.65 * lambert.max(0.0) / sun[1]).min(1.35)
}

/// Shaded map from a per-land-cell base colour.
fn shaded(w: &World, land_color: impl Fn(usize) -> [f64; 3]) -> Image {
    let n = w.n;
    let mut img = Image::new(n as u32, n as u32);
    for z in 0..n {
        for x in 0..n {
            let i = z * n + x;
            let (c, s) = if w.ocean[i] { (sea_color(-w.h[i]), 1.0) } else { (land_color(i), shade(w, x, z)) };
            let px = |v: f64| (v * s).clamp(0.0, 255.0) as u8;
            img.put_pixel(x as u32, z as u32, Rgb([px(c[0]), px(c[1]), px(c[2])]));
        }
    }
    img
}

fn relief_color(h: f64) -> [f64; 3] {
    if h < 1.5 {
        return [222.0, 205.0, 150.0];
    }
    ramp(
        &[
            (0.0, [96.0, 168.0, 84.0]),
            (30.0, [70.0, 140.0, 66.0]),
            (80.0, [120.0, 130.0, 80.0]),
            (140.0, [140.0, 120.0, 100.0]),
            (200.0, [170.0, 165.0, 160.0]),
            (250.0, [245.0, 245.0, 250.0]),
        ],
        h,
    )
}

/// Lakes as cells, rivers as anti-aliased polylines sized by Strahler order.
fn draw_water(img: &mut Image, w: &World, a: &Analysis) {
    let n = w.n;
    for i in 0..n * n {
        if !w.ocean[i] && a.lake_depth[i] >= LAKE_MIN_DEPTH_M {
            img.put_pixel((i % n) as u32, (i / n) as u32, Rgb(LAKE_RGB));
        }
    }
    let mut rivers: Vec<_> = a.rivers.iter().collect();
    rivers.sort_by_key(|r| r.order);
    for river in rivers {
        // Thinner on coarse grids, where a pixel already spans 32 m or more.
        let radius = (RIVER_RADIUS_PX.0 + RIVER_RADIUS_PX.1 * river.order as f64)
            * (16.0 / w.dx).clamp(0.5, 1.5);
        for pair in river.points.windows(2) {
            let (p, q) = (pair[0], pair[1]);
            if p.in_lake > 0.5 && q.in_lake > 0.5 {
                continue;
            }
            let (px, pz, qx, qz) = (p.x / w.dx, p.z / w.dx, q.x / w.dx, q.z / w.dx);
            let steps = (((qx - px).powi(2) + (qz - pz).powi(2)).sqrt() * 2.0).ceil().max(1.0) as usize;
            for s in 0..=steps {
                let t = s as f64 / steps as f64;
                let (cx, cz) = (px + (qx - px) * t, pz + (qz - pz) * t);
                let r = radius.ceil() as i64 + 1;
                for oz in -r..=r {
                    for ox in -r..=r {
                        let (x, z) = (cx.floor() as i64 + ox, cz.floor() as i64 + oz);
                        if x < 0 || z < 0 || x >= n as i64 || z >= n as i64 {
                            continue;
                        }
                        let d = ((x as f64 + 0.5 - cx).powi(2) + (z as f64 + 0.5 - cz).powi(2)).sqrt();
                        let alpha = (radius + 0.5 - d).clamp(0.0, 1.0);
                        if alpha <= 0.0 || w.ocean[z as usize * n + x as usize] {
                            continue;
                        }
                        let old = img.get_pixel(x as u32, z as u32).0;
                        // Max-blend toward the river colour so overlapping stamps don't darken.
                        let mixed = [0, 1, 2].map(|k| {
                            let target = RIVER_RGB[k];
                            let cur = old[k] as f64;
                            (cur + (target - cur) * alpha).clamp(0.0, 255.0) as u8
                        });
                        img.put_pixel(x as u32, z as u32, Rgb(mixed));
                    }
                }
            }
        }
    }
}

fn relief_image(w: &World, a: Option<&Analysis>) -> Image {
    let mut img = shaded(w, |i| relief_color(w.h[i]));
    if let Some(a) = a {
        draw_water(&mut img, w, a);
    }
    img
}

fn geology_image(w: &World, a: &Analysis) -> Image {
    let mut img = shaded(w, |i| ROCKS[a.surface_rock[i] as usize].rgb.map(|c| c as f64));
    draw_water(&mut img, w, a);
    img
}

fn rainfall_image(w: &World, a: &Analysis, rain: &[f64]) -> Image {
    let mut img = shaded(w, |i| {
        ramp(
            &[
                (0.2, [226.0, 200.0, 140.0]),
                (0.7, [190.0, 200.0, 120.0]),
                (1.2, [90.0, 170.0, 90.0]),
                (2.2, [30.0, 120.0, 110.0]),
                (3.5, [20.0, 70.0, 120.0]),
            ],
            rain[i],
        )
    });
    draw_water(&mut img, w, a);
    img
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::{forge, ForgeParams};

    fn tiny() -> crate::forge::Forged {
        let mut p = ForgeParams { size_m: 4_096.0, cell_m: 128.0, quiet: true, ..Default::default() };
        p.knobs.iters = 20.0;
        forge(&p)
    }

    #[test]
    fn render_is_n_by_n_rgba() {
        let f = tiny();
        let n = f.world.n;
        for layer in PreviewLayer::ALL {
            let img = render(&f, layer);
            assert_eq!((img.width as usize, img.height as usize), (n, n), "{}", layer.label());
            assert_eq!(img.pixels.len(), n * n * 4);
        }
    }

    #[test]
    fn relief_sea_and_land_differ() {
        let f = tiny();
        let n = f.world.n;
        let img = render(&f, PreviewLayer::Relief);
        let land = (0..n * n).filter(|&i| !f.world.ocean[i]).max_by(|&a, &b| f.world.h[a].total_cmp(&f.world.h[b])).unwrap();
        let px = |i: usize| img.pixels[i * 4..i * 4 + 4].to_vec();
        assert_ne!(px(0), px(land));
    }

    #[test]
    fn png_round_trips() {
        let f = tiny();
        let img = render(&f, PreviewLayer::Geology);
        let back = image::load_from_memory(&encode_png(&img)).unwrap();
        assert_eq!((back.width(), back.height()), (img.width, img.height));
    }

    #[test]
    fn stats_wrap_analysis() {
        let f = tiny();
        let s = stats(&f);
        assert_eq!(s.river_reaches, f.analysis.rivers.len());
        assert_eq!(s.land_share, f.analysis.stats.land_share);
    }
}
