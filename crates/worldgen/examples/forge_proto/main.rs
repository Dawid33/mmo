//! World forge prototype (F1): a whole finite world — one "super region",
//! by default the 16 × 8 km dev Planet at 32 m cells — generated OFFLINE by simulating landscape
//! evolution instead of evaluating noise per point. Touches no engine code and
//! no world output: it writes PNGs, a river/lake object file and statistics,
//! to find out what the real forge should be.
//! Background: docs/superpowers/specs/2026-09-20-df-worldgen-research/00-synthesis.md
//! (addendum) and docs/superpowers/specs/2026-09-20-world-forge-f0/findings.md.
//!
//! Run: just forge-proto                          (32 m cells, ~5 s)
//!      just forge-proto --cell 16                (full detail, ~30 s)
//!      just forge-proto --cell 64 --size medium  (a 100 × 50 km world)
//!      just forge-report --size dev --seeds 4    (forge in-process, print the
//!                                                 report's numbers, no PNGs)
//!      just forge-proto --search 64              (rank 64 seeds at 32 m)
//!      just forge-planet --seeds 8 --out <stem>  (the plate simulation on the
//!                                                 planet grid: its lines and
//!                                                 contact sheets, no forge)
//!      just forge-proto --no-geology --no-climate   (the F0 behaviour)
//!      just forge-ui                             (tuning page: every knob as a
//!                                                 slider, live map preview)
//!
//! Pipeline:
//!   1. world.rs    the Planet's continents (polar seas north and south, east
//!                  and west wrap), low initial relief, an UPLIFT map with
//!                  mountain belts (plates.rs);
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

mod planet_view;
mod render;
use worldgen::forge::render::PreviewLayer;
mod report;
mod search;
mod serve;

use std::ops::{Deref, DerefMut};
use std::path::PathBuf;

use worldgen::forge::hydrology;
use worldgen::forge::knobs;
use worldgen::forge::play;
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
    /// Override the forged spawn point (artifact metres, `--spawn X,Z`):
    /// region (0, 0) of the engine lands there, so a benchmark world can put
    /// its viewer in the middle of the land instead of where `spawn::pick`
    /// chose.
    pub spawn: Option<[f64; 2]>,
    /// `--report`: print the report's lines ([`report::report`]) instead of
    /// writing PNGs.
    pub report: bool,
    /// `--seeds N`: how many seeds `--report` forges (`seed`, `seed + 1`, …).
    pub seeds: usize,
    /// `--out` was given (`--report` writes a world file only then).
    pub out_given: bool,
    /// `--planet`: run the plate simulation alone ([`planet_view::run`]).
    pub planet: bool,
    /// `--frames n`: with `--planet`, `n` evenly spaced frames of the first seed.
    pub frames: usize,
    /// `--key <name>=<value>`: `PlanetKey` fields set by name (until Task 7
    /// makes them knobs), checked against their ranges at parse time.
    pub planet_keys: Vec<(String, f64)>,
    /// `--no-retry`: with `--planet`, run attempt 0 only instead of retrying.
    pub no_retry: bool,
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
            spawn: None,
            report: false,
            seeds: 1,
            out_given: false,
            planet: false,
            frames: 0,
            planet_keys: Vec::new(),
            no_retry: false,
        };
        let (mut width, mut height) = (None, None);
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            let mut value = || it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
            match flag.as_str() {
                "--size" => {
                    let size = play::size_flag(&value()).unwrap_or_else(|e| panic!("{e}"));
                    (width, height) = (Some(size.width_m), Some(size.height_m));
                }
                "--width" => width = Some(value().parse().expect("--width <meters>")),
                "--height" => height = Some(value().parse().expect("--height <meters>")),
                "--report" => p.report = true,
                "--planet" => p.planet = true,
                "--no-retry" => p.no_retry = true,
                "--frames" => p.frames = value().parse().expect("--frames <count>"),
                "--key" => p.planet_keys.push(planet_view::parse_key(&value()).unwrap_or_else(|e| panic!("{e}"))),
                "--seeds" => p.seeds = value().parse().expect("--seeds <count>"),
                "--cell" => p.cell_m = value().parse().expect("--cell <meters>"),
                "--iters" => p.knobs.iters = value().parse().expect("--iters <count>"),
                "--seed" => {
                    let v = value();
                    let hex = v.trim_start_matches("0x").replace('_', "");
                    p.seed = u64::from_str_radix(&hex, 16).expect("--seed <hex>");
                }
                "--out" => {
                    p.out = PathBuf::from(value());
                    p.out_given = true;
                }
                "--dt" => p.knobs.dt_years = value().parse().expect("--dt <years>"),
                "--uplift" => p.knobs.uplift = value().parse().expect("--uplift <m/yr>"),
                "--k" => p.knobs.erodibility = value().parse().expect("--k <erodibility>"),
                "--diffusivity" => p.knobs.diffusivity = value().parse().expect("--diffusivity <m^2/yr>"),
                "--no-geology" => p.knobs.geology_strength = 0.0,
                "--no-climate" => p.knobs.climate_strength = 0.0,
                "--spawn" => {
                    let v = value();
                    let (x, z) = v.split_once(',').expect("--spawn <x>,<z> (metres)");
                    p.spawn = Some([x.parse().expect("--spawn x"), z.parse().expect("--spawn z")]);
                }
                "--search" => p.search = value().parse().expect("--search <seed count>"),
                "--serve" => p.serve = Some(value().parse().expect("--serve <port>")),
                // A generator preset's knobs (forge::presets, by name, any
                // case): replaces every knob, so give it before any --set.
                "--preset" => {
                    let name = value();
                    let preset = worldgen::forge::presets::PRESETS
                        .iter()
                        .find(|p| p.name.eq_ignore_ascii_case(&name))
                        .unwrap_or_else(|| panic!("--preset: no preset named {name} (see forge/presets.rs)"));
                    p.knobs = worldgen::forge::presets::apply(preset);
                }
                // Any knob by its key (knobs.rs; the tuning page's "Copy as flags").
                // In the knob's range: load holds a stored knob to it.
                "--set" => p.knobs.set_flag(&value()).unwrap_or_else(|e| panic!("{e}")),
                other => panic!(
                    "unknown argument {other}; flags: --size <name> | --width <m> --height <m>, --cell --iters --seed --out --dt \
                     --uplift --k --diffusivity --no-geology --no-climate --preset <name> --set <knob>=<value> \
                     --search <n> --serve <port> --spawn <x>,<z> --report [--seeds <n>] \
                     --planet [--no-retry] [--seeds <n>] [--frames <n>] [--key <name>=<value>]"
                ),
            }
        }
        (p.width_m, p.height_m) = play::sides_flags(width, height, (p.width_m, p.height_m)).unwrap_or_else(|e| panic!("{e}"));
        p
    }
}


fn main() {
    let p = Params::parse();
    if p.planet {
        if let Some(dir) = p.out.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).expect("create output directory");
        }
        planet_view::run(&p);
        return;
    }
    if p.report {
        if p.out_given {
            if let Some(dir) = p.out.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).expect("create output directory");
            }
        }
        report::run(&p);
        return;
    }
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
    let (nx, nz) = (p.nx(), p.nz());
    println!(
        "forge: seed {:#018X}, {} x {} m world, {} m cells ({nx}x{nz}), {} iterations x {} yr over {} level(s), rock contrast {}, rain shadow {}",
        p.seed, p.width_m, p.height_m, p.cell_m,
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
    let deepest_s = (0..w.cells())
        .filter(|&i| !w.ocean[i])
        .map(|i| w.h[i] - w.uplifted[i] - forged.geology.bedding[i])
        .fold(f64::MAX, f64::min);
    println!(
        "simulated peak {:.0} m (height fit x{:.3}); deepest exhumed level s = {deepest_s:.0} m",
        forged.simulated_peak_m, forged.height_fit
    );
    print_climate(&forged, &p.forge);
    let started = std::time::Instant::now();
    let mut artifact = forged.artifact(&p.forge);
    let artifact_s = started.elapsed().as_secs_f64();
    if let Some(spawn) = p.spawn {
        artifact.spawn = spawn;
    }
    let (score, terms) = search::score(&forged, &artifact);
    println!("world score {score:.3}:");
    for (name, value) in terms {
        println!("  {name:<15} {value:.2}");
    }

    let stem = p.out.to_string_lossy().to_string();
    for (suffix, which) in [
        ("initial", PreviewLayer::BeforeErosion),
        ("relief", PreviewLayer::Relief),
        ("rivers", PreviewLayer::Rivers),
        ("geology", PreviewLayer::Geology),
        ("rain", PreviewLayer::Precipitation),
        ("temperature", PreviewLayer::Temperature),
        ("drainage", PreviewLayer::Drainage),
        ("regions", PreviewLayer::Regions),
    ] {
        render::layer(&forged, which, &format!("{stem}-{suffix}.png"));
    }
    render::heightmap(w, &format!("{stem}-height.png"));
    let objects = format!("{stem}-water.json");
    std::fs::write(&objects, hydrology::objects_json(w, a)).expect("write water objects");
    println!("wrote {objects} ({} lakes, {} river reaches)", a.lakes.len(), a.rivers.len());

    print_biomes(&artifact);
    print_rivers_and_peaks(&forged, &artifact);
    render::biomes(&artifact, &format!("{stem}-biome.png"));
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
    // The wall time of `forged.artifact` (the monthly climate included).
    println!("artifact: {artifact_s:.2} s");
}

/// The climate in eight latitude bands (north to south): wind, land
/// temperature and precipitation ranges.
fn print_climate(forged: &worldgen::forge::Forged, p: &worldgen::forge::ForgeParams) {
    use worldgen::forge::climate::{wind, Wind};
    let (w, c) = (&forged.world, &forged.climate);
    let (nx, nz) = (w.nx, w.nz);
    let land: Vec<usize> = (0..w.cells()).filter(|&i| !w.ocean[i]).collect();
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
        let rows = b * nz / BANDS..(b + 1) * nz / BANDS;
        let lat = c.latitude_deg[(rows.start + rows.end) / 2];
        let cells: Vec<usize> = land.iter().copied().filter(|i| rows.contains(&(i / nx))).collect();
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

/// Rivers by Strahler order (count, km), the land temperature and
/// precipitation ranges of the world file, and the peaks above the snow line.
fn print_rivers_and_peaks(forged: &worldgen::forge::Forged, a: &worldgen::forged::WorldArtifact) {
    use worldgen::forged::SNOW_LINE_C;
    let top = forged.analysis.rivers.iter().map(|r| r.order).max().unwrap_or(0);
    let by_order: Vec<String> = (1..=top)
        .rev()
        .map(|o| {
            let rs = forged.analysis.rivers.iter().filter(|r| r.order == o);
            let km: f64 = rs.clone().map(|r| r.points.windows(2).map(|w| ((w[1].x - w[0].x).hypot(w[1].z - w[0].z)) / 1000.0).sum::<f64>()).sum();
            format!("order {o}: {} ({km:.0} km)", rs.count())
        })
        .collect();
    println!("rivers by Strahler order: {}", by_order.join(", "));
    let land: Vec<usize> = (0..a.biome.len()).filter(|&i| a.height.get(i) > 0.0).collect();
    let range = |r: &worldgen::forged::QRaster| {
        land.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &i| (lo.min(r.get(i)), hi.max(r.get(i))))
    };
    let (t, r) = (range(&a.temperature_c), range(&a.precip_mm));
    println!("world file land: temperature {:.1}..{:.1} C, precipitation {:.0}..{:.0} mm/yr", t.0, t.1, r.0, r.1);
    let s = worldgen::forge::quality::summarise(a);
    println!(
        "peaks above the snow line (temperature < {SNOW_LINE_C} C): {}; mountain regions bordering other land: {}",
        s.peaks_above_snow, s.mountain_regions_bordering_land
    );
}

/// Biome shares (of all cells, and of land), region counts per type, and
/// the alignment and savagery split.
fn print_biomes(a: &worldgen::forged::WorldArtifact) {
    use worldgen::forge::regions::region_counts;
    let total = a.biome.len() as f64;
    println!("biomes (share of all cells / of land):");
    for (b, share, land) in report::biome_shares_with_land(a) {
        let land = land.map_or_else(|| String::from("     -"), |land| format!("{:5.1}%", 100.0 * land));
        println!("  {:<26} {:5.1}%  {land}", b.name(), 100.0 * share);
    }
    let counts = region_counts(&a.regions);
    let list: Vec<String> = counts.iter().map(|(t, c)| format!("{} {c}", t.name())).collect();
    let min = worldgen::forge::regions::min_region_cells(a.cell_m);
    let under = a.regions.iter().filter(|r| r.cells < min).count();
    println!("regions: {} ({}); {under} under the merge area (water or frozen with no same-class neighbour)", a.regions.len(), list.join(", "));
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
