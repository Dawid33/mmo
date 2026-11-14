//! `mc-import report|import|split <file.schem>…`: turns Sponge schematics into prefab files.
//! Run from the repo root (the `just mc-import` recipe does): it needs `assets/blocks/blocks.ron`.
//! Output goes to `prefab-imports/<file stem>/` by default, never under `assets/`.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use game::prefab::{prefab_to_bin, resolve, Trust, BIN_SUFFIX};
use game::registry::BlockRegistry;
use mc_import::{
    convert, read_schem, slug, split, AirMode, Build, BlockVolume, ConvertOptions, GroundMode, Report, SplitOptions,
    Table,
};

const BLOCKS_RON: &str = "assets/blocks/blocks.ron";
const DEFAULT_OUT: &str = "prefab-imports";
const REPORT_SUFFIX: &str = ".report.txt";
const TOP_UNMAPPED: usize = 10;
const USAGE: &str = "usage:
  mc-import report <file.schem>...
  mc-import import <file.schem> [--name N] [--out DIR] [--air auto|carve|keep] [--licence TEXT] [--author TEXT] [--tag T]...
  mc-import split <file.schem> [--prefix P] [--out DIR] [--gap N] [--min-blocks N] [--ground auto|none|N]
                  [--air auto|carve|keep] [--licence TEXT] [--author TEXT] [--tag T]...";

type Failure = (u8, String);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(code),
        Err((code, message)) => {
            eprintln!("{message}");
            ExitCode::from(code)
        }
    }
}

/// What the command line asked for (the flags of every subcommand; each rejects the others').
struct Opts {
    files: Vec<String>,
    name: Option<String>,
    out: Option<String>,
    gap: Option<u32>,
    min_blocks: Option<u64>,
    ground: Option<GroundMode>,
    air: AirMode,
    licence: String,
    author: Option<String>,
    tags: Vec<String>,
}

fn parse(cmd: &str, args: &[String]) -> Result<Opts, Failure> {
    let usage = |m: String| -> Failure { (2, format!("{m}\n{USAGE}")) };
    let mut o = Opts {
        files: Vec::new(),
        name: None,
        out: None,
        gap: None,
        min_blocks: None,
        ground: None,
        air: AirMode::Auto,
        licence: String::new(),
        author: None,
        tags: Vec::new(),
    };
    let name_flag = if cmd == "split" { "--prefix" } else { "--name" };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| it.next().cloned().ok_or_else(|| usage(format!("{flag} needs a value")));
        match a.as_str() {
            f if f == name_flag && cmd != "report" => o.name = Some(value(f)?),
            "--out" if cmd != "report" => o.out = Some(value("--out")?),
            "--air" if cmd != "report" => {
                o.air = match value("--air")?.as_str() {
                    "auto" => AirMode::Auto,
                    "carve" => AirMode::Carve,
                    "keep" => AirMode::Keep,
                    v => return Err(usage(format!("--air: \"{v}\" is not auto, carve or keep"))),
                }
            }
            "--licence" if cmd != "report" => o.licence = value("--licence")?,
            "--author" if cmd != "report" => o.author = Some(value("--author")?),
            "--tag" if cmd != "report" => o.tags.push(value("--tag")?),
            "--gap" if cmd == "split" => {
                let v = value("--gap")?;
                o.gap = Some(v.parse().map_err(|_| usage(format!("--gap: \"{v}\" is not a whole number")))?);
            }
            "--min-blocks" if cmd == "split" => {
                let v = value("--min-blocks")?;
                o.min_blocks = Some(v.parse().map_err(|_| usage(format!("--min-blocks: \"{v}\" is not a whole number")))?);
            }
            "--ground" if cmd == "split" => {
                o.ground = Some(match value("--ground")?.as_str() {
                    "auto" => GroundMode::Auto,
                    "none" => GroundMode::None,
                    v => GroundMode::Layers(
                        v.parse().map_err(|_| usage(format!("--ground: \"{v}\" is not auto, none or a number")))?,
                    ),
                });
            }
            flag if flag.starts_with('-') => return Err(usage(format!("unknown flag {flag}"))),
            _ => o.files.push(a.clone()),
        }
    }
    if o.files.is_empty() || (cmd != "report" && o.files.len() != 1) {
        return Err((2, USAGE.to_string()));
    }
    Ok(o)
}

fn run(args: &[String]) -> Result<u8, Failure> {
    let cmd = match args.first().map(String::as_str) {
        Some(c @ ("report" | "import" | "split")) => c,
        _ => return Err((2, USAGE.to_string())),
    };
    let opts = parse(cmd, &args[1..])?;
    let registry = load_registry()?;
    let mut failed = false;
    for file in &opts.files {
        let result = match cmd {
            "report" => report(file),
            _ => write_prefabs(cmd == "split", file, &opts, &registry),
        };
        match result {
            Ok(ok) => failed |= !ok,
            Err((2, m)) => return Err((2, m)),
            Err((_, m)) => {
                eprintln!("{file}: {m}");
                failed = true;
            }
        }
    }
    Ok(u8::from(failed))
}

fn load_registry() -> Result<BlockRegistry, Failure> {
    let src = std::fs::read_to_string(BLOCKS_RON)
        .map_err(|e| (2, format!("cannot read {BLOCKS_RON}: {e} (run mc-import from the repository root)")))?;
    BlockRegistry::from_ron(&src).map_err(|e| (2, format!("{BLOCKS_RON}: {e}")))
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

fn file_stem(path: &str) -> String {
    Path::new(path).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The file's bytes and the volume read from them (the bytes are dropped on return).
fn load(path: &str) -> Result<(BlockVolume, String), Failure> {
    let bytes = std::fs::read(path).map_err(|e| (1, format!("cannot read: {e}")))?;
    let hash = format!("{:08x}", crc32fast::hash(&bytes));
    let volume = read_schem(&bytes).map_err(|e| (1, e.to_string()))?;
    Ok((volume, hash))
}

fn non_air_cells(v: &BlockVolume) -> u64 {
    let solid: Vec<bool> =
        v.palette.iter().map(|s| !matches!(s.name.as_str(), "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air")).collect();
    v.blocks.iter().filter(|&&i| solid.get(i as usize).copied().unwrap_or(false)).count() as u64
}

fn report(path: &str) -> Result<bool, Failure> {
    let (v, hash) = load(path)?;
    let table = Table::builtin();
    let sp = split(&v, table, &SplitOptions::default());
    let [x, y, z] = v.size;
    println!(
        "{}  {x}\u{d7}{y}\u{d7}{z}  data {}  author \"{}\"  non-air {}  ground {}  builds {}",
        file_name(path),
        v.data_version,
        v.author,
        non_air_cells(&v),
        sp.ground_layers,
        sp.builds.len()
    );
    let opts = ConvertOptions {
        name: "report".into(),
        air: AirMode::Keep,
        region: None,
        author: v.author.clone(),
        source: file_name(path),
        licence: String::new(),
        source_hash: hash,
        tags: Vec::new(),
    };
    match convert(&v, table, &opts) {
        Some((_, r)) => {
            let mut top: Vec<(&String, &u64)> = r.unmapped.iter().collect();
            top.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            if !top.is_empty() {
                let list: Vec<String> =
                    top.iter().take(TOP_UNMAPPED).map(|(k, n)| format!("{k} \u{d7}{n}")).collect();
                println!("  unmapped ({} kinds): {}", top.len(), list.join(", "));
            }
            if !r.dropped.is_empty() {
                let list: Vec<String> = r.dropped.iter().map(|(k, n)| format!("{k} \u{d7}{n}")).collect();
                println!("  dropped: {}", list.join(", "));
            }
            if r.signs_unparsed > 0 {
                println!("  sign lines unparsed: {}", r.signs_unparsed);
            }
        }
        None => println!("  nothing convertible"),
    }
    Ok(true)
}

/// `--out` as an absolute, lexically normalised path (no `..`), to see whether it lies under `assets`.
fn under_assets(out: &Path) -> bool {
    let abs = if out.is_absolute() { out.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(out) };
    let mut parts: Vec<Component> = Vec::new();
    for c in abs.components() {
        match c {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            c => parts.push(c),
        }
    }
    parts.iter().any(|c| matches!(c, Component::Normal(n) if n.eq_ignore_ascii_case("assets")))
}

/// The prefab names of a split: `<prefix>-<slug of the label>`, else `<prefix>-<nn>` by build position.
fn split_names(prefix: &str, builds: &[Build]) -> Vec<String> {
    let width = if builds.len() >= 100 { 3 } else { 2 };
    let mut taken: BTreeSet<String> = BTreeSet::new();
    builds
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let labelled = b.label.as_deref().map(slug).filter(|s| !s.is_empty()).map(|s| format!("{prefix}-{s}"));
            let name = match labelled {
                Some(n) if !taken.contains(&n) => n,
                _ => format!("{prefix}-{:0width$}", i + 1),
            };
            taken.insert(name.clone());
            name
        })
        .collect()
}

fn warn(message: String) {
    eprintln!("warning: {message}");
}

fn write_prefabs(is_split: bool, path: &str, o: &Opts, registry: &BlockRegistry) -> Result<bool, Failure> {
    let stem = file_stem(path);
    let out_dir = match &o.out {
        Some(d) => PathBuf::from(d),
        None => Path::new(DEFAULT_OUT).join(&stem),
    };
    if under_assets(&out_dir) {
        return Err((2, format!("{}: refusing to write under assets/ (imports go to {DEFAULT_OUT}/)", out_dir.display())));
    }
    let (v, hash) = load(path)?;
    let table = Table::builtin();
    let base = slug(o.name.as_deref().unwrap_or(&stem));
    let base = if base.is_empty() { "prefab".to_string() } else { base };

    // (name, region) per prefab to write.
    let mut jobs: Vec<(String, Option<([u32; 3], [u32; 3])>)> = Vec::new();
    if is_split {
        let mut so = SplitOptions::default();
        if let Some(g) = o.gap {
            so.gap = g;
        }
        if let Some(m) = o.min_blocks {
            so.min_blocks = m;
        }
        let ground = o.ground.unwrap_or(GroundMode::Auto);
        so.ground = ground;
        let sp = split(&v, table, &so);
        let auto = ground == GroundMode::Auto;
        if sp.builds.is_empty() {
            warn(format!("{}: split found no builds", file_name(path)));
        }
        if auto && sp.ground_layers == v.size[1] {
            warn(format!(
                "{}: automatic ground detection took all {} layers; try --ground none",
                file_name(path),
                v.size[1]
            ));
        } else if auto && sp.ground_layers > 0 && sp.builds.len() == 1 {
            warn(format!(
                "{}: automatic ground took {} layers and one build is left (a single monument on a plate?); try --ground none or import",
                file_name(path),
                sp.ground_layers
            ));
        }
        let names = split_names(&base, &sp.builds);
        for i in 0..sp.builds.len() {
            for j in i + 1..sp.builds.len() {
                let (a, b) = (&sp.builds[i], &sp.builds[j]);
                if a.min[0] < b.max[0] && b.min[0] < a.max[0] && a.min[2] < b.max[2] && b.min[2] < a.max[2] {
                    warn(format!("builds {} and {} overlap in x/z", names[i], names[j]));
                }
            }
        }
        for (n, b) in names.into_iter().zip(&sp.builds) {
            jobs.push((n, Some((b.min, b.max))));
        }
    } else {
        jobs.push((base, None));
    }

    let mut tags = o.tags.clone();
    tags.push("imported".to_string());
    let mut ok = true;
    let mut made_dir = false;
    for (name, region) in jobs {
        let opts = ConvertOptions {
            name: name.clone(),
            air: o.air,
            region,
            author: o.author.clone().unwrap_or_else(|| v.author.clone()),
            source: file_name(path),
            licence: o.licence.clone(),
            source_hash: hash.clone(),
            tags: tags.clone(),
        };
        let Some((file, report)) = convert(&v, table, &opts) else {
            eprintln!("{name}: nothing to convert, skipped");
            continue;
        };
        if !made_dir {
            std::fs::create_dir_all(&out_dir).map_err(|e| (2, format!("{}: {e}", out_dir.display())))?;
            made_dir = true;
        }
        let bin = out_dir.join(format!("{name}{BIN_SUFFIX}"));
        let txt = out_dir.join(format!("{name}{REPORT_SUFFIX}"));
        std::fs::write(&bin, prefab_to_bin(&file)).map_err(|e| (1, format!("{}: {e}", bin.display())))?;
        std::fs::write(&txt, report.to_text(&name)).map_err(|e| (1, format!("{}: {e}", txt.display())))?;
        summary_line(&name, &file.size, &report, file.markers.len());
        let unmapped: u64 = report.unmapped.values().sum();
        if unmapped > 0 || report.signs_unparsed > 0 {
            warn(format!("{name}: {unmapped} unmapped cells, {} unparsed sign lines", report.signs_unparsed));
        }
        if let Err(problems) = resolve(&file, &|n| registry.id_of(n), Trust::Builtin) {
            eprintln!("{name}: does not resolve, {} problem(s):", problems.len());
            for p in &problems {
                eprintln!("  {p}");
            }
            ok = false;
        }
    }
    Ok(ok)
}

fn summary_line(name: &str, size: &[u32; 3], r: &Report, markers: usize) {
    let unmapped: u64 = r.unmapped.values().sum();
    println!(
        "{name}  {}x{}x{}  {} blocks  {} boxes  {} shaped  {markers} markers  {unmapped} unmapped",
        size[0], size[1], size[2], r.blocks, r.boxes, r.shaped
    );
}

