//! World forge prototype (F1): a whole finite world — one "super region",
//! 16 km, ocean all round — generated OFFLINE by simulating landscape
//! evolution instead of evaluating noise per point. Touches no engine code and
//! no world output: it writes PNGs, a river/lake object file and statistics,
//! to find out what the real forge should be.
//! Background: docs/superpowers/specs/2026-09-20-df-worldgen-research/00-synthesis.md
//! (addendum) and docs/superpowers/specs/2026-09-20-world-forge-f0/findings.md.
//!
//! Run: just forge-proto                          (16 m cells, ~30 s)
//!      just forge-proto --cell 32                (quick look, ~5 s)
//!      just forge-proto --search 64              (rank 64 seeds at 32 m)
//!      just forge-proto --no-geology --no-climate   (the F0 behaviour)
//!      just forge-ui                             (tuning page: every knob as a
//!                                                 slider, live map preview)
//!
//! Pipeline:
//!   1. world.rs    noise island inside a guaranteed ocean margin, low initial
//!                  relief, an UPLIFT map with mountain belts inland;
//!   2. geology.rs  rock column in a stratigraphic coordinate (tilt + folds,
//!                  domed by the uplift) -> erodibility of whatever rock the
//!                  surface currently cuts: geology BEFORE erosion;
//!   3. climate.rs  orographic rain on the current heights -> discharge weights;
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
        "forge: seed {:#018X}, {} m world, {} m cells ({n}x{n}), {} iterations x {} yr, rock contrast {}, rain shadow {}",
        p.seed, p.size_m, p.cell_m, p.knobs.iterations(), p.knobs.dt_years,
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
