//! World forge prototype (F1): a whole finite world — one "super region",
//! by default a 16 km continent at 32 m cells — generated OFFLINE by simulating landscape
//! evolution instead of evaluating noise per point. Touches no engine code and
//! no world output: it writes PNGs, a river/lake object file and statistics,
//! to find out what the real forge should be.
//! Background: docs/superpowers/specs/2026-09-20-df-worldgen-research/00-synthesis.md
//! (addendum) and docs/superpowers/specs/2026-09-20-world-forge-f0/findings.md.
//!
//! Run: just forge-proto                          (32 m cells, ~5 s)
//!      just forge-proto --cell 16                (full detail, ~30 s)
//!      just forge-proto --cell 64 --size 100352  (a ~100 km continent, ~15 s)
//!      just forge-proto --search 64              (rank 64 seeds at 32 m)
//!      just forge-proto --no-geology --no-climate   (the F0 behaviour)
//!      just forge-ui                             (tuning page: every knob as a
//!                                                 slider, live map preview)
//!
//! Pipeline:
//!   1. world.rs    the layout's landmass (islands in a guaranteed ocean margin,
//!                  or a continent with sea and land edges), low initial
//!                  relief, an UPLIFT map with mountain belts (plates.rs on
//!                  the continental layouts);
//!   2. geology.rs  rock column in a stratigraphic coordinate (tilt + folds,
//!                  domed by the uplift) -> erodibility of whatever rock the
//!                  surface currently cuts: geology BEFORE erosion;
//!   3. climate.rs  latitude, temperature, banded-wind precipitation (mm/yr)
//!                  on the current heights -> discharge weights;
//!   4. erosion.rs  per iteration: Priority-Flood routing, discharge, implicit
//!                  stream-power incision, depression silting, hillslope diffusion;
//!   5. post.rs     valley floors planed beside rivers; overdeepened basins
//!                  flooded to their true spill level = lakes;
//!   6. hydrology.rs  lakes and rivers as objects (Strahler order, width,
//!                  monotone water surface, smoothed polylines) + statistics;
//!   7. search.rs   generate-and-test over seeds.
//!   knobs.rs holds every tunable (CLI `--set key=value`); serve.rs + ui.html
//!   are the local tuning page.
//!
//! Not bit-reproducible across machines by design (sqrt/ln/sin used freely):
//! the forge's output is a stored artifact, not a function both sides evaluate.

mod render;
use worldgen::forge::render::PreviewLayer;
mod search;
mod serve;

use std::ops::{Deref, DerefMut};
use std::path::PathBuf;

use worldgen::forge::hydrology;
use worldgen::forge::knobs;
use worldgen::forge::{forge, ForgeParams};

const DEFAULT_OUT: &str = "target/world-forge/f1";

/// The forge's parameters plus what only the command line decides.
#[derive(Clone)]
pub struct Params {
    pub forge: ForgeParams,
    pub out: PathBuf,
    /// Try this many seeds at 32 m cells and rank them instead of forging one.
    pub search: usize,
    /// Serve the tuning page on this port instead of forging once.
    pub serve: Option<u16>,
}

impl Deref for Params {
    type Target = ForgeParams;
    fn deref(&self) -> &ForgeParams {
        &self.forge
    }
}

impl DerefMut for Params {
    fn deref_mut(&mut self) -> &mut ForgeParams {
        &mut self.forge
    }
}

impl Params {
    pub fn parse() -> Params {
        let mut p = Params {
            forge: ForgeParams { quiet: false, ..ForgeParams::default() },
            out: PathBuf::from(DEFAULT_OUT),
            search: 0,
            serve: None,
        };
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            let mut value = || it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
            match flag.as_str() {
                "--size" => p.size_m = value().parse().expect("--size <meters>"),
                "--cell" => p.cell_m = value().parse().expect("--cell <meters>"),
                "--iters" => p.knobs.iters = value().parse().expect("--iters <count>"),
                "--seed" => {
                    let v = value();
                    let hex = v.trim_start_matches("0x").replace('_', "");
                    p.seed = u64::from_str_radix(&hex, 16).expect("--seed <hex>");
                }
                "--out" => p.out = PathBuf::from(value()),
                "--dt" => p.knobs.dt_years = value().parse().expect("--dt <years>"),
                "--uplift" => p.knobs.uplift = value().parse().expect("--uplift <m/yr>"),
                "--k" => p.knobs.erodibility = value().parse().expect("--k <erodibility>"),
                "--diffusivity" => p.knobs.diffusivity = value().parse().expect("--diffusivity <m^2/yr>"),
                "--no-geology" => p.knobs.geology_strength = 0.0,
                "--no-climate" => p.knobs.climate_strength = 0.0,
                "--search" => p.search = value().parse().expect("--search <seed count>"),
                "--serve" => p.serve = Some(value().parse().expect("--serve <port>")),
                // Any knob by its key (knobs.rs; the tuning page's "Copy as flags").
                "--set" => {
                    let v = value();
                    let (key, number) = v.split_once('=').expect("--set <knob>=<value>");
                    let number: f64 = number.parse().unwrap_or_else(|_| panic!("--set {key}: not a number"));
                    assert!(p.knobs.set(key, number), "--set: no knob named {key} (see knobs.rs)");
                }
                other => panic!(
                    "unknown argument {other}; flags: --size --cell --iters --seed --out --dt \
                     --uplift --k --diffusivity --no-geology --no-climate --set <knob>=<value> \
                     --search <n> --serve <port>"
                ),
            }
        }
        p
    }
}


fn main() {
    let p = Params::parse();
    if let Some(dir) = p.out.parent() {
        std::fs::create_dir_all(dir).expect("create output directory");
    }
    if let Some(port) = p.serve {
        serve::run(&p, port);
        return;
    }
    if p.search > 0 {
        search::run(&p);
        return;
    }
    let n = (p.size_m / p.cell_m).round() as usize;
    println!(
        "forge: seed {:#018X}, {} m world, {} m cells ({n}x{n}), {} iterations x {} yr over {} level(s), rock contrast {}, rain shadow {}",
        p.seed, p.size_m, p.cell_m,
        worldgen::forge::schedule(&p).iter().map(|&(_, i)| i).sum::<usize>(), p.knobs.dt_years,
        worldgen::forge::schedule(&p).len(),
        p.knobs.geology_strength, p.knobs.climate_strength,
    );
    let changed: Vec<String> = knobs::SPECS
        .iter()
        .filter_map(|s| {
            let v = p.knobs.get(s.key).unwrap();
            (v != s.default).then(|| format!("{}={v}", s.key))
        })
        .collect();
    if !changed.is_empty() {
        println!("knobs changed from their defaults: {}", changed.join(" "));
    }
    let forged = forge(&p.forge);
    let (w, a) = (&forged.world, &forged.analysis);
    hydrology::print_report("forged world", w.dx, &a.stats);
    // The deepest stratigraphic level the surface cuts: what the geology
    // column must reach (geology::COLUMN_BOTTOM_S).
    let deepest_s = (0..w.n * w.n)
        .filter(|&i| !w.ocean[i])
        .map(|i| w.h[i] - w.uplifted[i] - forged.geology.bedding[i])
        .fold(f64::MAX, f64::min);
    println!(
        "simulated peak {:.0} m (height fit x{:.3}); deepest exhumed level s = {deepest_s:.0} m",
        forged.simulated_peak_m, forged.height_fit
    );
    print_climate(&forged, &p.forge);
    println!("world score {:.3}:", search::score(&a.stats, w.n * w.n));
    for (name, value) in search::criteria(&a.stats, w.n * w.n) {
        println!("  {name:<13} {value:.2}");
    }

    let stem = p.out.to_string_lossy().to_string();
    for (suffix, which) in [
        ("initial", PreviewLayer::BeforeErosion),
        ("relief", PreviewLayer::Relief),
        ("rivers", PreviewLayer::Rivers),
        ("geology", PreviewLayer::Geology),
        ("rain", PreviewLayer::Rain),
    ] {
        render::layer(&forged, which, &format!("{stem}-{suffix}.png"));
    }
    render::heightmap(w, &format!("{stem}-height.png"));
    let objects = format!("{stem}-water.json");
    std::fs::write(&objects, hydrology::objects_json(w, a)).expect("write water objects");
    println!("wrote {objects} ({} lakes, {} river reaches)", a.lakes.len(), a.rivers.len());

    let artifact = forged.artifact(&p.forge);
    print_biomes(&artifact);
    let world_file = format!("{stem}.world");
    artifact.save(std::path::Path::new(&world_file)).expect("write world file");
    let bytes = std::fs::metadata(&world_file).map(|m| m.len()).unwrap_or(0);
    println!(
        "wrote {world_file} ({:.1} MiB), spawn at ({:.0}, {:.0}) m — serve it with {}={world_file}",
        bytes as f64 / (1024.0 * 1024.0),
        artifact.spawn[0],
        artifact.spawn[1],
        worldgen::forged::WORLD_FILE_ENV
    );
}

/// The climate in eight latitude bands (north to south): wind, land
/// temperature and precipitation ranges.
fn print_climate(forged: &worldgen::forge::Forged, p: &worldgen::forge::ForgeParams) {
    use worldgen::forge::climate::{wind, Wind};
    let (w, c) = (&forged.world, &forged.climate);
    let n = w.n;
    let land: Vec<usize> = (0..n * n).filter(|&i| !w.ocean[i]).collect();
    let range = |v: &[f32], cells: &[usize]| {
        cells.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &i| (lo.min(v[i]), hi.max(v[i])))
    };
    let (t, r) = (range(&c.temperature_c, &land), range(&c.precip_mm, &land));
    println!(
        "climate: land temperature {:.1}..{:.1} C, precipitation {:.0}..{:.0} mm/yr",
        t.0, t.1, r.0, r.1
    );
    const BANDS: usize = 8;
    for b in 0..BANDS {
        let rows = b * n / BANDS..(b + 1) * n / BANDS;
        let lat = c.latitude_deg[(rows.start + rows.end) / 2];
        let cells: Vec<usize> = land.iter().copied().filter(|i| rows.contains(&(i / n))).collect();
        let dir = if wind(lat as f64, p) == Wind::FromWest { "W" } else { "E" };
        if cells.is_empty() {
            println!("  lat {lat:5.1}  wind from {dir}  (sea)");
            continue;
        }
        let mean = |v: &[f32]| cells.iter().map(|&i| v[i] as f64).sum::<f64>() / cells.len() as f64;
        let (t, r) = (range(&c.temperature_c, &cells), range(&c.precip_mm, &cells));
        println!(
            "  lat {lat:5.1}  wind from {dir}  land T {:5.1} ({:5.1}..{:5.1}) C  precip {:5.0} ({:4.0}..{:4.0}) mm/yr",
            mean(&c.temperature_c), t.0, t.1, mean(&c.precip_mm), r.0, r.1
        );
    }
}

/// Biome shares (of all cells, and of land), region counts per type, and
/// the alignment and savagery split.
fn print_biomes(a: &worldgen::forged::WorldArtifact) {
    use worldgen::forge::regions::{biome_shares, region_counts};
    use game::Biome;
    let water = |b: Biome| matches!(b, Biome::Ocean | Biome::FrozenOcean | Biome::Lake);
    let land_cells = a.biome.iter().filter(|&&b| !Biome::from_u8(b).is_some_and(water)).count().max(1) as f64;
    let total = a.biome.len() as f64;
    println!("biomes (share of all cells / of land):");
    for (b, share) in biome_shares(&a.biome) {
        let land = if water(b) { String::from("     -") } else { format!("{:5.1}%", 100.0 * share * total / land_cells) };
        println!("  {:<26} {:5.1}%  {land}", b.name(), 100.0 * share);
    }
    let counts = region_counts(&a.regions);
    let list: Vec<String> = counts.iter().map(|(t, c)| format!("{} {c}", t.name())).collect();
    println!("regions: {} ({})", a.regions.len(), list.join(", "));
    let cells = |al: i8| a.regions.iter().filter(|r| r.alignment == al).map(|r| r.cells as f64).sum::<f64>();
    let aligned: f64 = a.regions.iter().filter(|r| r.kind != game::RegionType::Ocean).map(|r| r.cells as f64).sum::<f64>().max(1.0);
    println!(
        "alignment (share of non-ocean cells): good {:.1}% ({} regions), evil {:.1}% ({} regions)",
        100.0 * cells(1) / aligned,
        a.regions.iter().filter(|r| r.alignment == 1).count(),
        100.0 * cells(-1) / aligned,
        a.regions.iter().filter(|r| r.alignment == -1).count(),
    );
    let third = |lo: u8, hi: u8| 100.0 * a.savagery.iter().filter(|&&s| (lo..=hi).contains(&s)).count() as f64 / total;
    println!("savagery: calm {:.0}%, neutral {:.0}%, savage {:.0}%", third(0, 32), third(33, 66), third(67, 100));
}
